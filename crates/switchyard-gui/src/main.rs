#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod file_watcher;
mod git;
mod pty;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex as StdMutex};
use tauri::Emitter;
use tauri::Manager;

use file_watcher::{CapturedChange, FileWatcherState};

use switchyard_config::{SandboxMode, SwitchyardConfig};
use switchyard_core::{
    ProviderRegistry, build_peer_catalog_probed, execution_policy_from_config_with_overrides,
    run_routed_turn_observable_with_policy_and_attachments,
};
use switchyard_provider_antigravity::AntigravityProvider;
use switchyard_provider_api::{
    CancellationToken, HostSurfaceProbe, InputAttachment, LiveInstanceRegistry, Provider,
};
use switchyard_provider_claude::ClaudeProvider;
use switchyard_provider_codex::CodexProvider;
use switchyard_provider_gemini::GeminiProvider;
use switchyard_session::{Artifact, Event, Session, Turn, Workspace};
use switchyard_store::{
    ArtifactStore, SessionCatalog, SessionEventRepository, SessionRepository, StoreBackend,
    StoreHandle, TurnRepository, WorkspaceIndex, default_index_path, workspace_data_dir,
};

fn get_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Tauri-managed state holding the in-memory copy of `workspaces.json`.
/// Mutations go through the state's lock; writes back to disk happen at
/// every command boundary so a crash doesn't lose user-level state.
struct WorkspaceState {
    index: StdMutex<WorkspaceIndex>,
    index_path: PathBuf,
    /// Cached git repo root for the *current* workspace. Resolved
    /// lazily on first access and invalidated whenever the active
    /// workspace changes (or its `primary_root` is updated). Stored
    /// here rather than recomputed per file-op call because every
    /// SourceControl click would otherwise spawn a `git rev-parse`.
    git_repo_root: StdMutex<GitRepoCache>,
}

/// One cached repo-root resolution. `workspace_id` lets us detect a
/// stale cache after a workspace switch without plumbing an
/// invalidation hook into every mutator.
struct GitRepoCache {
    workspace_id: Option<uuid::Uuid>,
    repo_root: Option<PathBuf>,
}

impl GitRepoCache {
    fn empty() -> Self {
        Self {
            workspace_id: None,
            repo_root: None,
        }
    }
}

impl WorkspaceState {
    /// Load the index on app startup, creating a "Default" workspace
    /// rooted at the process cwd if the index is empty. The same path
    /// the old `get_cwd()`-based commands assumed.
    fn load_or_bootstrap() -> Result<Self, String> {
        let index_path = default_index_path();
        let mut index = WorkspaceIndex::load(&index_path)
            .map_err(|e| format!("failed to load workspace index: {e}"))?;
        if index.workspaces.is_empty() {
            let mut default_ws = Workspace::new(get_cwd());
            default_ws.name = "Default".to_string();
            index.insert(default_ws);
            index
                .save(&index_path)
                .map_err(|e| format!("failed to seed default workspace: {e}"))?;
        }
        Ok(Self {
            index: StdMutex::new(index),
            index_path,
            git_repo_root: StdMutex::new(GitRepoCache::empty()),
        })
    }

    /// Resolve (and cache) the git repo root for the active workspace.
    /// Returns `None` if the workspace isn't inside a git repository
    /// — callers can still proceed with workspace-only scope.
    fn git_repo_root(&self) -> Option<PathBuf> {
        let current = self.current().ok()?;
        let mut cache = self.git_repo_root.lock().ok()?;
        if cache.workspace_id == Some(current.workspace_id) {
            return cache.repo_root.clone();
        }
        let resolved = git::repo_root(&current.primary_root).ok();
        cache.workspace_id = Some(current.workspace_id);
        cache.repo_root = resolved.clone();
        resolved
    }

    /// Clear the cached repo root. Called from the workspace mutator
    /// commands whenever primary_root could have shifted.
    fn invalidate_git_cache(&self) {
        if let Ok(mut cache) = self.git_repo_root.lock() {
            *cache = GitRepoCache::empty();
        }
    }

    /// Read-side helper that copies out the current workspace; returns
    /// `Err` when no workspaces exist (impossible after `load_or_bootstrap`
    /// but kept defensive for tests).
    fn current(&self) -> Result<Workspace, String> {
        let guard = self.index.lock().map_err(|_| "workspace state poisoned")?;
        guard
            .current_workspace()
            .cloned()
            .ok_or_else(|| "no workspace selected".to_string())
    }

    /// Apply `f` to the index, write to disk, return whatever `f` returned.
    fn mutate<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut WorkspaceIndex) -> Result<R, String>,
    {
        let mut guard = self.index.lock().map_err(|_| "workspace state poisoned")?;
        let out = f(&mut guard)?;
        guard
            .save(&self.index_path)
            .map_err(|e| format!("failed to persist workspace index: {e}"))?;
        Ok(out)
    }
}

/// Open the canonical store for a given workspace. The store path is
/// derived from `workspace_data_dir(workspace_id)` so each workspace's
/// data lives at `~/.switchyard/workspaces/<id>/` regardless of the
/// workspace's `primary_root`. Returns (store, store_path) — the latter
/// is useful for artifact / job directories which sit next to the store.
fn open_workspace_store(
    workspace: &Workspace,
    config: &SwitchyardConfig,
) -> Result<(StoreHandle, PathBuf), String> {
    let data_dir = workspace_data_dir(workspace.workspace_id);
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("failed to create workspace data dir: {e}"))?;
    // Backend selection still honours the user's switchyard.toml choice;
    // the path is overridden to the per-workspace dir.
    let backend = match config.store.backend {
        switchyard_config::StoreBackendConfig::Sqlite => StoreBackend::Sqlite,
        switchyard_config::StoreBackendConfig::Jsonl => StoreBackend::Jsonl,
        switchyard_config::StoreBackendConfig::Auto => {
            // Default to sqlite for new workspaces; existing dirs with a
            // sessions/ subfolder prefer jsonl for backward compat.
            if data_dir.join("sessions").exists() {
                StoreBackend::Jsonl
            } else {
                StoreBackend::Sqlite
            }
        }
    };
    let store_path = match backend {
        StoreBackend::Jsonl => data_dir.join("sessions"),
        StoreBackend::Sqlite => data_dir.join("store.sqlite3"),
    };
    let store = StoreHandle::open(backend, store_path.clone())
        .map_err(|e| format!("failed to open workspace store: {e}"))?;
    Ok((store, data_dir))
}

/// Open the store for the currently-active workspace. Most commands take
/// this shape: open store → do work → drop. Returns `(workspace, store,
/// data_dir, config)` so callers don't re-resolve repeatedly.
fn open_current_store(
    state: &tauri::State<'_, WorkspaceState>,
) -> Result<(Workspace, StoreHandle, PathBuf, SwitchyardConfig), String> {
    let ws = state.current()?;
    let config = SwitchyardConfig::resolve(&ws.primary_root).unwrap_or_default();
    let (store, data_dir) = open_workspace_store(&ws, &config)?;
    Ok((ws, store, data_dir, config))
}

// ===========================================================================
// Workspace Tauri commands
// ===========================================================================

#[tauri::command]
fn list_workspaces(
    workspace_state: tauri::State<'_, WorkspaceState>,
) -> Result<Vec<Workspace>, String> {
    let guard = workspace_state
        .index
        .lock()
        .map_err(|_| "workspace state poisoned")?;
    Ok(guard.workspaces.clone())
}

#[tauri::command]
fn get_current_workspace(
    workspace_state: tauri::State<'_, WorkspaceState>,
) -> Result<Option<Workspace>, String> {
    let guard = workspace_state
        .index
        .lock()
        .map_err(|_| "workspace state poisoned")?;
    Ok(guard.current_workspace().cloned())
}

#[tauri::command]
fn open_external_terminal(cwd: String) -> Result<(), String> {
    let cwd_path = PathBuf::from(&cwd);
    if !cwd_path.is_dir() {
        return Err(format!("cwd is not a directory: {cwd}"));
    }
    spawn_external_terminal(&cwd_path)
}

#[cfg(target_os = "windows")]
fn spawn_external_terminal(cwd: &Path) -> Result<(), String> {
    // Prefer Windows Terminal when available; it gives the closest native
    // VS Code-like fallback if the embedded terminal is not enough for a TUI.
    match ProcessCommand::new("wt.exe").arg("-d").arg(cwd).spawn() {
        Ok(_) => return Ok(()),
        Err(wt_err) => {
            // Fall back to the inbox Windows PowerShell. `-NoExit` keeps the
            // window open and `Set-Location -LiteralPath` handles spaces and
            // special characters in project paths.
            let ps_cwd = quote_powershell_literal(cwd);
            ProcessCommand::new("powershell.exe")
                .arg("-NoExit")
                .arg("-Command")
                .arg(format!("Set-Location -LiteralPath {ps_cwd}"))
                .spawn()
                .map(|_| ())
                .map_err(|ps_err| {
                    format!(
                        "failed to open external terminal (wt.exe: {wt_err}; powershell.exe: {ps_err})"
                    )
                })
        }
    }
}

#[cfg(target_os = "windows")]
fn quote_powershell_literal(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

#[cfg(target_os = "macos")]
fn spawn_external_terminal(cwd: &Path) -> Result<(), String> {
    ProcessCommand::new("open")
        .arg("-a")
        .arg("Terminal")
        .arg(cwd)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to open Terminal.app: {e}"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_external_terminal(cwd: &Path) -> Result<(), String> {
    let attempts = [
        (
            "x-terminal-emulator",
            vec!["--working-directory".to_string(), cwd.display().to_string()],
        ),
        (
            "gnome-terminal",
            vec![format!("--working-directory={}", cwd.display())],
        ),
        (
            "konsole",
            vec!["--workdir".to_string(), cwd.display().to_string()],
        ),
    ];
    let mut errors = Vec::new();
    for (program, args) in attempts {
        match ProcessCommand::new(program).args(args).spawn() {
            Ok(_) => return Ok(()),
            Err(e) => errors.push(format!("{program}: {e}")),
        }
    }
    Err(format!(
        "failed to open external terminal; tried {}",
        errors.join("; ")
    ))
}

#[tauri::command]
fn set_current_workspace(
    workspace_state: tauri::State<'_, WorkspaceState>,
    file_watcher: tauri::State<'_, FileWatcherState>,
    workspace_id: String,
) -> Result<Workspace, String> {
    let id =
        uuid::Uuid::parse_str(&workspace_id).map_err(|e| format!("invalid workspace ID: {}", e))?;
    let new_ws = workspace_state.mutate(|idx| {
        idx.set_current(id)
            .map_err(|e| format!("set_current: {e}"))?;
        idx.get(id)
            .cloned()
            .ok_or_else(|| format!("workspace {id} not found"))
    })?;
    // A different workspace means a different (possibly absent) git
    // repo. Drop the cached repo root so the next file-op resolves
    // fresh.
    workspace_state.invalidate_git_cache();
    // Re-point the file watcher at the new workspace's roots. Failure
    // here shouldn't block the switch — just log; the user can still
    // chat, they'll only miss AI-change capture until next switch.
    if let Err(e) = file_watcher.watch_workspace(&new_ws) {
        eprintln!("[file_watcher] watch_workspace on switch failed: {e}");
    }
    Ok(new_ws)
}

#[tauri::command]
fn create_workspace(
    workspace_state: tauri::State<'_, WorkspaceState>,
    primary_root: String,
    name: Option<String>,
) -> Result<Workspace, String> {
    let root = PathBuf::from(&primary_root);
    if !root.is_dir() {
        return Err(format!("primary_root is not a directory: {}", primary_root));
    }
    workspace_state.mutate(|idx| {
        let mut ws = Workspace::new(root);
        if let Some(n) = name {
            ws.name = n;
        }
        let id = idx.insert(ws);
        Ok(idx
            .get(id)
            .cloned()
            .expect("just-inserted workspace must exist"))
    })
}

#[tauri::command]
fn update_workspace(
    workspace_state: tauri::State<'_, WorkspaceState>,
    file_watcher: tauri::State<'_, FileWatcherState>,
    workspace_id: String,
    name: Option<String>,
    extra_roots: Option<Vec<String>>,
) -> Result<Workspace, String> {
    let id =
        uuid::Uuid::parse_str(&workspace_id).map_err(|e| format!("invalid workspace ID: {}", e))?;
    let extra_roots_changed = extra_roots.is_some();
    // Rename doesn't touch roots, but extra_roots changes do — and a
    // future "edit primary_root" affordance would too. Conservatively
    // drop the git-root cache on any update so we never serve a stale
    // repo root.
    workspace_state.invalidate_git_cache();
    let updated = workspace_state.mutate(|idx| {
        let ws = idx
            .get_mut(id)
            .ok_or_else(|| format!("workspace {id} not found"))?;
        if let Some(n) = name {
            ws.name = n;
        }
        if let Some(roots) = extra_roots {
            ws.extra_roots = roots.into_iter().map(PathBuf::from).collect();
        }
        ws.updated_at = chrono::Utc::now();
        Ok(ws.clone())
    })?;
    // If the user changed extra_roots and this is the active workspace,
    // the watcher needs to know about the new directories. Re-watching
    // is cheap (a clear + fresh notify::Watcher) so we do it whenever
    // roots could have shifted.
    if extra_roots_changed {
        let current_id = workspace_state
            .index
            .lock()
            .ok()
            .and_then(|g| g.current_workspace().map(|w| w.workspace_id));
        if current_id == Some(id) {
            if let Err(e) = file_watcher.watch_workspace(&updated) {
                eprintln!("[file_watcher] re-watch on update failed: {e}");
            }
        }
    }
    Ok(updated)
}

#[tauri::command]
fn delete_workspace(
    workspace_state: tauri::State<'_, WorkspaceState>,
    workspace_id: String,
) -> Result<(), String> {
    let id =
        uuid::Uuid::parse_str(&workspace_id).map_err(|e| format!("invalid workspace ID: {}", e))?;
    workspace_state.mutate(|idx| {
        idx.remove(id);
        Ok(())
    })
    // Note: we intentionally don't delete the workspace's on-disk data
    // dir here. That's a separate destructive op the user can do
    // manually if they want — keeping it around protects against
    // accidental "remove workspace from index" mistakes.
}

// ===========================================================================
// Hook installer Tauri commands
// ===========================================================================
//
// Thin wrappers over the `switchyard host hook` CLI logic so the GUI
// can install/uninstall/inspect Codex + Claude hooks without shelling
// out. The actual install logic lives in switchyard-cli's host_hook
// module — we duplicate the same primitives here so the GUI doesn't
// depend on the cli crate's bin layout.

#[derive(serde::Serialize)]
struct GuiHookStatus {
    codex_config_path: PathBuf,
    codex_installed_events: Vec<String>,
    claude_config_path: PathBuf,
    claude_installed_events: Vec<String>,
}

/// Match the hook event set the CLI installer uses, so the GUI and
/// CLI flows produce identical entries.
const GUI_HOOK_EVENTS: &[&str] = &[
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "SessionStart",
];
const GUI_HOOK_MANAGED_MARKER: &str = "switchyard_managed";

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn codex_hooks_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
        .join("config.toml")
}

fn claude_hooks_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("hooks.json")
}

fn current_exe_cmd() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "switchyard".to_string())
}

fn install_codex_hooks() -> std::io::Result<()> {
    let path = codex_hooks_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut root: toml::Value = if existing.trim().is_empty() {
        toml::Value::Table(Default::default())
    } else {
        existing.parse::<toml::Value>().map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("parse: {e}"))
        })?
    };
    let exe_path = current_exe_cmd();
    let root_table = root.as_table_mut().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "config.toml root is not a table",
        )
    })?;
    let hooks_table = root_table
        .entry("hooks")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "`hooks` is not a table")
        })?;

    for event in GUI_HOOK_EVENTS {
        let entries = hooks_table
            .entry(event.to_string())
            .or_insert_with(|| toml::Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("hooks.{event} is not an array"),
                )
            })?;
        entries.retain(|v| {
            v.as_table()
                .and_then(|t| t.get(GUI_HOOK_MANAGED_MARKER))
                .and_then(|m| m.as_bool())
                != Some(true)
        });
        let mut entry = toml::value::Table::new();
        entry.insert("type".into(), toml::Value::String("command".into()));
        entry.insert(
            "command".into(),
            toml::Value::String(format!(
                "{exe_path} host hook fire --provider codex --event {event}"
            )),
        );
        entry.insert(
            "description".into(),
            toml::Value::String("switchyard:managed".into()),
        );
        entry.insert(GUI_HOOK_MANAGED_MARKER.into(), toml::Value::Boolean(true));
        entries.push(toml::Value::Table(entry));
    }
    let serialized = toml::to_string_pretty(&root).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("serialize: {e}"))
    })?;
    std::fs::write(&path, serialized)?;
    Ok(())
}

fn uninstall_codex_hooks() -> std::io::Result<()> {
    let path = codex_hooks_path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let mut root: toml::Value = existing
        .parse::<toml::Value>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("parse: {e}")))?;
    let mut changed = false;
    if let Some(root_table) = root.as_table_mut()
        && let Some(hooks_table) = root_table.get_mut("hooks").and_then(|h| h.as_table_mut())
    {
        for (_event, entries) in hooks_table.iter_mut() {
            if let Some(arr) = entries.as_array_mut() {
                let before = arr.len();
                arr.retain(|v: &toml::Value| {
                    v.as_table()
                        .and_then(|t| t.get(GUI_HOOK_MANAGED_MARKER))
                        .and_then(|m| m.as_bool())
                        != Some(true)
                });
                if arr.len() != before {
                    changed = true;
                }
            }
        }
        hooks_table.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    }
    if changed {
        let serialized = toml::to_string_pretty(&root).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("serialize: {e}"))
        })?;
        std::fs::write(&path, serialized)?;
    }
    Ok(())
}

fn read_codex_installed() -> std::io::Result<Vec<String>> {
    let path = codex_hooks_path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let root: toml::Value = existing
        .parse::<toml::Value>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("parse: {e}")))?;
    let mut events = Vec::new();
    if let Some(hooks_table) = root.get("hooks").and_then(|h| h.as_table()) {
        for (event_name, entries) in hooks_table {
            if let Some(arr) = entries.as_array() {
                let any = arr.iter().any(|v| {
                    v.as_table()
                        .and_then(|t| t.get(GUI_HOOK_MANAGED_MARKER))
                        .and_then(|m| m.as_bool())
                        == Some(true)
                });
                if any {
                    events.push(event_name.clone());
                }
            }
        }
    }
    events.sort();
    Ok(events)
}

fn install_claude_hooks() -> std::io::Result<()> {
    let path = claude_hooks_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut root: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({"hooks": {}})
    } else {
        serde_json::from_str(&existing).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("parse: {e}"))
        })?
    };
    let exe_path = current_exe_cmd();
    let root_obj = root.as_object_mut().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "hooks.json root is not an object",
        )
    })?;
    let hooks = root_obj
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "`hooks` is not an object")
        })?;
    for event in GUI_HOOK_EVENTS {
        let arr = hooks
            .entry((*event).to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("hooks.{event} is not an array"),
                )
            })?;
        arr.retain(|v| {
            v.as_object()
                .and_then(|o| o.get(GUI_HOOK_MANAGED_MARKER))
                .and_then(|m| m.as_bool())
                != Some(true)
        });
        arr.push(serde_json::json!({
            "type": "command",
            "command": format!("{exe_path} host hook fire --provider claude --event {event}"),
            "description": "switchyard:managed",
            GUI_HOOK_MANAGED_MARKER: true,
        }));
    }
    let serialized = serde_json::to_string_pretty(&root)?;
    std::fs::write(&path, serialized)?;
    Ok(())
}

fn uninstall_claude_hooks() -> std::io::Result<()> {
    let path = claude_hooks_path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    if existing.trim().is_empty() {
        return Ok(());
    }
    let mut root: serde_json::Value = serde_json::from_str(&existing)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("parse: {e}")))?;
    let mut changed = false;
    if let Some(root_obj) = root.as_object_mut()
        && let Some(hooks) = root_obj.get_mut("hooks").and_then(|h| h.as_object_mut())
    {
        for (_event, entries) in hooks.iter_mut() {
            if let Some(arr) = entries.as_array_mut() {
                let before = arr.len();
                arr.retain(|v| {
                    v.as_object()
                        .and_then(|o| o.get(GUI_HOOK_MANAGED_MARKER))
                        .and_then(|m| m.as_bool())
                        != Some(true)
                });
                if arr.len() != before {
                    changed = true;
                }
            }
        }
        hooks.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    }
    if changed {
        let serialized = serde_json::to_string_pretty(&root)?;
        std::fs::write(&path, serialized)?;
    }
    Ok(())
}

fn read_claude_installed() -> std::io::Result<Vec<String>> {
    let path = claude_hooks_path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    if existing.trim().is_empty() {
        return Ok(Vec::new());
    }
    let root: serde_json::Value = serde_json::from_str(&existing)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("parse: {e}")))?;
    let mut events = Vec::new();
    if let Some(hooks) = root.get("hooks").and_then(|h| h.as_object()) {
        for (event_name, entries) in hooks {
            if let Some(arr) = entries.as_array() {
                let any = arr.iter().any(|v| {
                    v.as_object()
                        .and_then(|o| o.get(GUI_HOOK_MANAGED_MARKER))
                        .and_then(|m| m.as_bool())
                        == Some(true)
                });
                if any {
                    events.push(event_name.clone());
                }
            }
        }
    }
    events.sort();
    Ok(events)
}

#[tauri::command]
fn hook_install(provider: String) -> Result<(), String> {
    match provider.as_str() {
        "codex" => install_codex_hooks().map_err(|e| e.to_string()),
        "claude" => install_claude_hooks().map_err(|e| e.to_string()),
        "all" => {
            install_codex_hooks().map_err(|e| e.to_string())?;
            install_claude_hooks().map_err(|e| e.to_string())?;
            Ok(())
        }
        other => Err(format!(
            "unknown provider '{other}' (use codex, claude, or all)"
        )),
    }
}

#[tauri::command]
fn hook_uninstall(provider: String) -> Result<(), String> {
    match provider.as_str() {
        "codex" => uninstall_codex_hooks().map_err(|e| e.to_string()),
        "claude" => uninstall_claude_hooks().map_err(|e| e.to_string()),
        "all" => {
            uninstall_codex_hooks().map_err(|e| e.to_string())?;
            uninstall_claude_hooks().map_err(|e| e.to_string())?;
            Ok(())
        }
        other => Err(format!(
            "unknown provider '{other}' (use codex, claude, or all)"
        )),
    }
}

#[tauri::command]
fn hook_status() -> Result<GuiHookStatus, String> {
    Ok(GuiHookStatus {
        codex_config_path: codex_hooks_path(),
        codex_installed_events: read_codex_installed().map_err(|e| e.to_string())?,
        claude_config_path: claude_hooks_path(),
        claude_installed_events: read_claude_installed().map_err(|e| e.to_string())?,
    })
}

// ===========================================================================
// Filesystem Tauri commands (workspace-scoped)
// ===========================================================================
//
// Used by the Canvas (read_file) and the future Files mode (list_dir).
// All paths are resolved against the current workspace's roots and
// refused if they escape the declared scope. This is the only place
// the frontend can poke at the filesystem — every other path is
// confined to Switchyard's own data dirs.

/// Snapshot of a file at the time of read. `content` may be empty for
/// binary files we deliberately refuse to load; check `is_binary` in
/// that case to render a friendly placeholder.
#[derive(Debug, serde::Serialize)]
struct FileSnapshot {
    /// Path relative to the workspace's primary_root when possible;
    /// otherwise absolute. Stable identifier the UI uses as the tab key.
    path: String,
    /// File contents as UTF-8. Empty when `is_binary = true`.
    content: String,
    /// True when the file isn't valid UTF-8 — the Canvas should render
    /// a "binary file, N bytes" placeholder rather than the empty
    /// string. Lets us gate Phase 2 "open as hex" without a separate API.
    is_binary: bool,
    /// File size in bytes.
    size: u64,
    /// Best-effort language hint inferred from the extension. The Canvas
    /// status bar surfaces this; Phase 2 may wire it to a syntax
    /// highlighter.
    language: String,
}

/// Single entry returned by `list_dir`. Used by the Files mode tree.
#[derive(Debug, serde::Serialize)]
struct DirEntryView {
    name: String,
    /// Path relative to the workspace's primary_root when possible.
    path: String,
    is_dir: bool,
    size: u64,
}

/// Resolve a possibly-relative path against the workspace, refusing
/// anything that lands outside the allowed scope (defense against
/// `..`-style traversal even with absolute paths).
///
/// **Allowed scope** = workspace's `primary_root` + `extra_roots` +
/// (when the workspace is inside a git repository) the repo root.
/// The git-repo extension exists so the Source Control panel can
/// open / save / revert files anywhere in the repo even when the
/// workspace itself targets a subdirectory.
///
/// **Why not `Path::canonicalize`?** On Windows, `canonicalize` returns
/// paths prefixed with `\\?\` (the NT extended-length form). The
/// workspace roots are stored without that prefix, so a canonical-form
/// containment check rejects every file as "outside the workspace".
/// We use lexical normalization (collapse `.`, resolve `..` against
/// earlier components) which is enough to defend against traversal
/// while keeping the path shape consistent with the stored roots.
fn resolve_workspace_path(
    ws: &Workspace,
    path: &str,
    git_repo_root: Option<&Path>,
) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(path);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        ws.primary_root.join(candidate)
    };
    let normalized = lexical_normalize(&absolute);
    let workspace_roots: Vec<PathBuf> = ws
        .all_roots()
        .iter()
        .map(|r| lexical_normalize(r))
        .collect();
    let git_root_norm = git_repo_root.map(lexical_normalize);
    let contained = workspace_roots.iter().any(|r| normalized.starts_with(r))
        || git_root_norm
            .as_ref()
            .map(|r| normalized.starts_with(r))
            .unwrap_or(false);
    if !contained {
        return Err(format!(
            "path '{}' is outside workspace roots",
            normalized.display()
        ));
    }
    Ok(normalized)
}

/// Lexical normalization: collapse `.` segments, resolve `..` against
/// earlier components. Mirrors Go's `filepath.Clean` for the parts we
/// care about. Does NOT touch the filesystem — symlinks are followed
/// at read time by `tokio::fs::read`, which is fine since Switchyard
/// runs in a trusted user context.
fn lexical_normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[tauri::command]
async fn read_file(
    workspace_state: tauri::State<'_, WorkspaceState>,
    path: String,
) -> Result<FileSnapshot, String> {
    let ws = workspace_state.current()?;
    let repo_root = workspace_state.git_repo_root();
    let resolved = resolve_workspace_path(&ws, &path, repo_root.as_deref())?;
    let metadata = tokio::fs::metadata(&resolved)
        .await
        .map_err(|e| format!("stat {}: {e}", resolved.display()))?;
    if metadata.is_dir() {
        return Err(format!("{} is a directory", resolved.display()));
    }
    let size = metadata.len();
    let bytes = tokio::fs::read(&resolved)
        .await
        .map_err(|e| format!("read {}: {e}", resolved.display()))?;
    let (content, is_binary) = match String::from_utf8(bytes) {
        Ok(s) => (s, false),
        Err(_) => (String::new(), true),
    };

    // Report path relative to primary_root when possible — the Canvas
    // shows this in its tab bar and status line.
    let rel = resolved
        .strip_prefix(&ws.primary_root)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| resolved.to_string_lossy().to_string());

    Ok(FileSnapshot {
        path: rel,
        content,
        is_binary,
        size,
        language: infer_language(&resolved),
    })
}

#[tauri::command]
async fn write_file(
    workspace_state: tauri::State<'_, WorkspaceState>,
    path: String,
    content: String,
) -> Result<FileSnapshot, String> {
    let ws = workspace_state.current()?;
    let repo_root = workspace_state.git_repo_root();
    let resolved = resolve_workspace_path(&ws, &path, repo_root.as_deref())?;
    // Refuse to clobber a directory — Canvas only ever opens files,
    // but guard against the rare case where a user types a folder path
    // into a future "save as" dialog.
    if let Ok(meta) = tokio::fs::metadata(&resolved).await
        && meta.is_dir()
    {
        return Err(format!("{} is a directory", resolved.display()));
    }
    // Ensure parent exists. Lets the user save a brand-new file under
    // a folder Switchyard owns without first creating the folder
    // manually. We still refuse writes outside the workspace via
    // `resolve_workspace_path`, so this can't create arbitrary dirs.
    if let Some(parent) = resolved.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("create parent {}: {e}", parent.display()))?;
    }
    tokio::fs::write(&resolved, content.as_bytes())
        .await
        .map_err(|e| format!("write {}: {e}", resolved.display()))?;
    let size = content.len() as u64;
    let rel = resolved
        .strip_prefix(&ws.primary_root)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| resolved.to_string_lossy().to_string());
    Ok(FileSnapshot {
        path: rel,
        content,
        is_binary: false,
        size,
        language: infer_language(&resolved),
    })
}

#[tauri::command]
async fn list_dir(
    workspace_state: tauri::State<'_, WorkspaceState>,
    path: Option<String>,
) -> Result<Vec<DirEntryView>, String> {
    let ws = workspace_state.current()?;
    let repo_root = workspace_state.git_repo_root();
    // No path → list the primary_root. Lets the Files mode bootstrap
    // without the frontend needing to know where the workspace lives.
    let target = match path.as_deref() {
        Some(p) if !p.is_empty() => resolve_workspace_path(&ws, p, repo_root.as_deref())?,
        _ => ws.primary_root.clone(),
    };
    let metadata = tokio::fs::metadata(&target)
        .await
        .map_err(|e| format!("stat {}: {e}", target.display()))?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", target.display()));
    }
    let mut entries = Vec::new();
    let mut read = tokio::fs::read_dir(&target)
        .await
        .map_err(|e| format!("read_dir {}: {e}", target.display()))?;
    while let Some(entry) = read
        .next_entry()
        .await
        .map_err(|e| format!("dir iter: {e}"))?
    {
        let m = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let entry_path = entry.path();
        let rel = entry_path
            .strip_prefix(&ws.primary_root)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| entry_path.to_string_lossy().to_string());
        entries.push(DirEntryView {
            name: entry.file_name().to_string_lossy().to_string(),
            path: rel,
            is_dir: m.is_dir(),
            size: m.len(),
        });
    }
    // Dirs first, then files, both alphabetical — matches what most
    // file explorers do, no surprise for the user.
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

// ===========================================================================
// Source Control (git) Tauri commands
// ===========================================================================
//
// All scoped to the current workspace's `primary_root`. Each command
// resolves the git CLI fresh — no daemon, no persistent handle — so a
// crashed git won't poison subsequent calls.

#[tauri::command]
fn git_is_repo(workspace_state: tauri::State<'_, WorkspaceState>) -> Result<bool, String> {
    let ws = workspace_state.current()?;
    Ok(git::is_repo(&ws.primary_root))
}

#[tauri::command]
fn git_status(workspace_state: tauri::State<'_, WorkspaceState>) -> Result<git::GitStatus, String> {
    let ws = workspace_state.current()?;
    git::status(&ws.primary_root)
}

#[tauri::command]
fn git_file_diff(
    workspace_state: tauri::State<'_, WorkspaceState>,
    path: String,
    staged: bool,
) -> Result<git::GitFileDiff, String> {
    let ws = workspace_state.current()?;
    git::file_diff(&ws.primary_root, &path, staged)
}

#[tauri::command]
fn git_stage(
    workspace_state: tauri::State<'_, WorkspaceState>,
    path: String,
) -> Result<(), String> {
    let ws = workspace_state.current()?;
    git::stage(&ws.primary_root, &path)
}

#[tauri::command]
fn git_unstage(
    workspace_state: tauri::State<'_, WorkspaceState>,
    path: String,
) -> Result<(), String> {
    let ws = workspace_state.current()?;
    git::unstage(&ws.primary_root, &path)
}

#[tauri::command]
fn git_discard(
    workspace_state: tauri::State<'_, WorkspaceState>,
    path: String,
) -> Result<(), String> {
    let ws = workspace_state.current()?;
    git::discard(&ws.primary_root, &path)
}

#[tauri::command]
fn git_commit(
    workspace_state: tauri::State<'_, WorkspaceState>,
    message: String,
) -> Result<String, String> {
    let ws = workspace_state.current()?;
    git::commit(&ws.primary_root, &message)
}

#[tauri::command]
fn git_init(workspace_state: tauri::State<'_, WorkspaceState>) -> Result<(), String> {
    let ws = workspace_state.current()?;
    git::init(&ws.primary_root)
}

/// Auto-derive a session name from the user's first message. The output
/// targets ~40 chars / 6 words — enough to be recognisable in a list
/// without overflowing the Workspace column's session row.
///
/// Algorithm:
///   1. Strip leading punctuation / whitespace
///   2. Take the first line
///   3. Take the first 8 whitespace-separated words
///   4. Hard-cap at 60 characters with an ellipsis when truncated
fn derive_session_name(message: &str) -> String {
    // First non-empty line. Multiline prompts usually have the
    // interesting bit on line one; later lines are context.
    let first_line = message
        .lines()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())
        .unwrap_or("");
    // Drop a small set of leading-punctuation noise (common when users
    // dictate "- do X" or "## refactor Y"). Hand-rolled to avoid
    // pulling in a regex crate for one filter.
    let cleaned: String = first_line
        .trim_start_matches(|c: char| {
            matches!(c, '#' | '-' | '*' | '>' | '/' | '\\' | '`' | '"' | '\'') || c.is_whitespace()
        })
        .to_string();
    if cleaned.is_empty() {
        return String::new();
    }
    let words: Vec<&str> = cleaned.split_whitespace().take(8).collect();
    let joined = words.join(" ");
    if joined.chars().count() > 60 {
        let mut truncated: String = joined.chars().take(57).collect();
        // Trim trailing whitespace before adding the ellipsis so we
        // don't end up with "foo bar …" extra space.
        while truncated.ends_with(char::is_whitespace) {
            truncated.pop();
        }
        truncated.push('…');
        truncated
    } else {
        joined
    }
}

/// Map a file extension to a coarse language tag for the status bar /
/// Phase 2 highlighter. Lowercased extension lookup; unknown extensions
/// return `"plaintext"`.
fn infer_language(path: &Path) -> String {
    let ext: String = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s: &str| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        "php" => "php",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "swift" => "swift",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" => "css",
        "sh" | "bash" | "zsh" => "shell",
        "ps1" => "powershell",
        "bat" | "cmd" => "batch",
        "sql" => "sql",
        "xml" => "xml",
        "dockerfile" => "dockerfile",
        "" => "plaintext",
        other => other,
    }
    .to_string()
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
            } else if name.contains("antigravity") || name.contains("agy") {
                // Match "antigravity" / "agy" BEFORE the gemini check —
                // Antigravity shares Gemini's config tree under `~/.gemini/`
                // but the binary and protocol are different. Provider name
                // disambiguates.
                "antigravity"
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
            "antigravity" => {
                registry.register(
                    name.clone(),
                    Box::new(|cfg| {
                        let p: Box<dyn Provider> = match cfg {
                            Some(c) => Box::new(AntigravityProvider::from_config(c)),
                            None => Box::new(AntigravityProvider::new(
                                "agy",
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
    if !registry.has("antigravity") {
        registry.register(
            "antigravity",
            Box::new(|cfg| {
                let p: Box<dyn Provider> = match cfg {
                    Some(c) => Box::new(AntigravityProvider::from_config(c)),
                    None => Box::new(AntigravityProvider::new(
                        "agy",
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
        // Antigravity has no streaming IPC and no structured output yet; the
        // safe default is "worker" only — let the user opt it in to core
        // explicitly via config if they really want plain-text core.
        "antigravity" => vec!["worker".to_string()],
        _ => vec!["worker".to_string()],
    }
}

#[tauri::command]
async fn list_provider_status(
    workspace_state: tauri::State<'_, WorkspaceState>,
) -> Result<Vec<ProviderStatus>, String> {
    // Probe resolution honours the current workspace's `primary_root` so
    // provider configs colocated with the project (`./switchyard.toml`)
    // are picked up correctly.
    let ws = workspace_state.current()?;
    let cwd = ws.primary_root.clone();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let registry = build_registry(&config);
    let checked_at = chrono::Utc::now().to_rfc3339();

    let mut provider_names = BTreeSet::new();
    provider_names.extend([
        "codex".to_string(),
        "claude".to_string(),
        "gemini".to_string(),
        "antigravity".to_string(),
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
async fn load_config(
    workspace_state: tauri::State<'_, WorkspaceState>,
) -> Result<SwitchyardConfig, String> {
    let cwd = workspace_state
        .current()
        .map(|ws| ws.primary_root)
        .unwrap_or_else(|_| get_cwd());
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

        // Populate four basic CLIs. Upstream renamed the `*-cli`
        // binaries to their bare names in 2025 (OpenAI dropped the
        // `codex-cli` suffix, Anthropic ships as `claude`, Google's
        // Gemini CLI installs as `gemini`); we default to the new
        // names. The subprocess resolver's `alias_candidates` falls
        // back to the old `-cli` form if only the legacy binary is
        // present, so neither old nor new installs need manual
        // config tweaking.
        for name in &["codex", "claude", "gemini", "antigravity"] {
            if !final_config.providers.contains_key(*name) {
                let command = match *name {
                    "codex" => "codex",
                    "claude" => "claude",
                    "gemini" => "gemini",
                    "antigravity" => "agy",
                    _ => *name,
                };

                // No default args — each provider's turn-execution
                // code in `switchyard-provider-{codex,claude,gemini,
                // antigravity}` constructs the correct subcommand
                // (`codex exec --json`, `claude --print --output-format
                // stream-json`, etc.) and appends these as
                // *extra* args. Seeding `["run"]` was a hold-over
                // from the old `*-cli` binaries that nested under a
                // `run` subcommand; new releases drop it entirely and
                // misinterpret `run` as the prompt.
                let args: Vec<String> = Vec::new();

                final_config.providers.insert(
                    name.to_string(),
                    switchyard_config::ProviderConfig {
                        command: command.to_string(),
                        args,
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
async fn save_config(
    workspace_state: tauri::State<'_, WorkspaceState>,
    config: SwitchyardConfig,
) -> Result<(), String> {
    let cwd = workspace_state
        .current()
        .map(|ws| ws.primary_root)
        .unwrap_or_else(|_| get_cwd());
    let config_path = cwd.join("switchyard.toml");
    config
        .write_to(&config_path)
        .map_err(|e| format!("failed to save config: {}", e))?;
    Ok(())
}

#[tauri::command]
async fn list_sessions(
    workspace_state: tauri::State<'_, WorkspaceState>,
) -> Result<Vec<Session>, String> {
    let (ws, store, _data_dir, _config) = open_current_store(&workspace_state)?;
    let session_ids = store.list_sessions().map_err(|e| e.to_string())?;
    let mut sessions = Vec::new();
    for id in session_ids {
        if let Ok(Some(mut s)) = store.load_session(id) {
            // Stamp legacy sessions (workspace_id == nil) onto the current
            // workspace so they show up after migration without needing
            // a write-back here.
            if s.workspace_id.is_nil() {
                s.workspace_id = ws.workspace_id;
            }
            // Filter to the active workspace. After migration this should
            // be every session, but defensive filtering keeps cross-
            // workspace bleed impossible.
            if s.workspace_id == ws.workspace_id {
                sessions.push(s);
            }
        }
    }
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(sessions)
}

#[tauri::command]
async fn create_session(
    workspace_state: tauri::State<'_, WorkspaceState>,
    provider: String,
) -> Result<Session, String> {
    let (ws, mut store, _data_dir, _config) = open_current_store(&workspace_state)?;
    let session = Session::new_in_workspace(ws.workspace_id, provider);
    store
        .save_session(&session)
        .map_err(|e| format!("failed to save session: {}", e))?;
    Ok(session)
}

#[tauri::command]
async fn get_session_turns(
    workspace_state: tauri::State<'_, WorkspaceState>,
    session_id: String,
) -> Result<Vec<Turn>, String> {
    let (_ws, store, _data_dir, _config) = open_current_store(&workspace_state)?;
    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;
    let turns = store.list_turns(session_uuid).map_err(|e| e.to_string())?;
    Ok(turns)
}

#[tauri::command]
async fn get_session_events(
    workspace_state: tauri::State<'_, WorkspaceState>,
    session_id: String,
) -> Result<Vec<Event>, String> {
    let (_ws, store, _data_dir, _config) = open_current_store(&workspace_state)?;
    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;
    let events = store
        .list_session_events(session_uuid)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|event| !switchyard_provider_api::is_empty_reasoning_payload(&event.payload))
        .collect();
    Ok(events)
}

fn validate_image_attachments(
    cwd: &Path,
    image_paths: Vec<String>,
) -> Result<Vec<InputAttachment>, String> {
    image_paths
        .into_iter()
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| {
            let candidate = PathBuf::from(raw.trim());
            let absolute = if candidate.is_absolute() {
                candidate
            } else {
                cwd.join(candidate)
            };
            let normalized = lexical_normalize(&absolute);
            if !normalized.is_file() {
                return Err(format!(
                    "attached image not found: {}",
                    normalized.display()
                ));
            }
            let extension = normalized
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_ascii_lowercase())
                .ok_or_else(|| {
                    format!(
                        "attached image has no supported extension: {}",
                        normalized.display()
                    )
                })?;
            let mime_type = image_mime_type(&extension).ok_or_else(|| {
                format!(
                    "unsupported image extension '.{}' for {}",
                    extension,
                    normalized.display()
                )
            })?;
            Ok(InputAttachment {
                path: normalized,
                mime_type: Some(mime_type.to_string()),
            })
        })
        .collect()
}

fn image_mime_type(extension: &str) -> Option<&'static str> {
    match extension {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
        _ => None,
    }
}

#[tauri::command]
async fn run_turn(
    app: tauri::AppHandle,
    pool: tauri::State<'_, Arc<switchyard_core::InstancePool>>,
    workspace_state: tauri::State<'_, WorkspaceState>,
    file_watcher: tauri::State<'_, FileWatcherState>,
    session_id: String,
    message: String,
    provider: Option<String>,
    sandbox_mode: Option<SandboxMode>,
    image_paths: Option<Vec<String>>,
) -> Result<String, String> {
    let (ws, mut store, data_dir, config) = open_current_store(&workspace_state)?;
    let cwd = ws.primary_root.clone();
    let attachments = validate_image_attachments(&cwd, image_paths.unwrap_or_default())?;
    let registry = build_registry(&config);

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    let mut session = store
        .load_session(session_uuid)
        .map_err(|e| format!("load session: {}", e))?
        .ok_or_else(|| format!("session {} not found", session_id))?;

    // Late-bind legacy sessions (workspace_id == nil from migration) to
    // the current workspace.
    if session.workspace_id.is_nil() {
        session.workspace_id = ws.workspace_id;
        let _ = store.save_session(&session);
    }

    // Auto-name new sessions on their first turn. We only set the name
    // when:
    //   - The user hasn't picked a custom name yet (name.is_none())
    //   - There are no prior turns persisted for this session
    // The derived label is the first few words of the user message
    // trimmed to ~60 chars. A future enhancement could call an LLM to
    // summarize once the first turn finishes; this heuristic ships
    // value without a model round-trip.
    if session.name.is_none() {
        let existing_turns = store
            .list_turns(session.session_id)
            .map_err(|e| format!("list turns for naming: {e}"))?;
        if existing_turns.is_empty() {
            let derived = derive_session_name(&message);
            if !derived.is_empty() {
                session.name = Some(derived);
                let _ = store.save_session(&session);
            }
        }
    }

    let provider = provider.unwrap_or_else(|| session.active_core.clone());
    let _ = app.emit(
        "runtime_event",
        switchyard_core::RuntimeEvent::TurnPreparing {
            session_id: session.session_id,
            provider: provider.clone(),
            phase: "resolving provider and warming persistent instance".to_string(),
        },
    );

    let provider_impl = registry
        .create(&provider, config.providers.get(&provider))
        .ok_or_else(|| format!("unsupported provider: {}", provider))?;

    // Pre-spawn/ensure core provider is persistent for this session. Try to
    // resume a previously-bound CLI thread via the per-session
    // `native_bindings[<provider>_resume_token]` slot — `start_persistent_instance_resumed`
    // gracefully falls back to a fresh start if the daemon refuses the token.
    if let Some(persistent) = provider_impl.as_persistent()
        && !pool.has_live_instance(&provider, session.session_id)
    {
        let mut env = config
            .providers
            .get(&provider)
            .map(|c| c.env.clone())
            .unwrap_or_default();
        // Inject Switchyard identity so any hooks the CLI fires
        // (`switchyard host hook fire …`) can find the right session.
        env.insert(
            "SWITCHYARD_SESSION_ID".to_string(),
            session.session_id.to_string(),
        );
        env.insert("SWITCHYARD_PROVIDER".to_string(), provider.clone());
        let resume_key = format!("{provider}_resume_token");
        let resume_token = session.native_bindings.get(&resume_key).cloned();
        if let Ok(inst) = persistent
            .start_persistent_instance_resumed(cwd.clone(), env, resume_token.clone())
            .await
        {
            // After spawn, the live instance may have minted a fresh thread
            // (when the token was None, stale, or unsupported). Persist the
            // current token so the next respawn can resume cleanly.
            let new_token = inst.resume_token();
            match (&new_token, &resume_token) {
                (Some(t), Some(prior)) if t == prior => {
                    // Resume took — no save needed.
                }
                (Some(t), _) => {
                    session
                        .native_bindings
                        .insert(resume_key.clone(), t.clone());
                    let _ = store.save_session(&session);
                }
                (None, Some(_)) => {
                    // Token now invalid (provider stopped exposing one). Drop
                    // the binding so we don't keep retrying a stale id.
                    session.native_bindings.remove(&resume_key);
                    let _ = store.save_session(&session);
                }
                (None, None) => {}
            }

            let mut metadata = switchyard_provider_api::InstanceMetadata::new(
                provider.clone(),
                session.session_id,
                None,
                switchyard_provider_api::InstanceKind::Core,
            );
            metadata.state = switchyard_provider_api::InstanceState::Idle;
            let spawned_at = metadata.spawned_at;
            let provider_name = metadata.provider.clone();
            if let Ok(instance_id) = pool.register(metadata, inst) {
                // Emit a WorkerSpawned so the frontend can update the Core
                // status card without polling. Worker peers get their own
                // events via the supervisor; the Core spawns here outside
                // the supervisor path.
                let _ = app.emit(
                    "runtime_event",
                    switchyard_core::RuntimeEvent::WorkerSpawned {
                        session_id: session.session_id,
                        instance_id,
                        provider: provider_name,
                        label: None,
                        kind: "core".to_string(),
                        spawned_at: spawned_at.to_rfc3339(),
                    },
                );
            }
        }
    }

    let registry_dyn: Arc<dyn switchyard_provider_api::LiveInstanceRegistry> = pool.inner().clone();
    let core_proxy = switchyard_core::PersistentProviderProxy::new(
        provider.clone(),
        session.session_id,
        provider_impl,
        Some(registry_dyn.clone()),
    );

    let _ = app.emit(
        "runtime_event",
        switchyard_core::RuntimeEvent::TurnPreparing {
            session_id: session.session_id,
            provider: provider.clone(),
            phase: "probing peer catalog and opening runtime bridge".to_string(),
        },
    );

    let peer_catalog = build_peer_catalog_probed(&provider, &registry, &config.providers).await;
    // Artifacts live next to the workspace's store so they move together
    // when the user copies the workspace dir or wipes it.
    let artifact_dir = data_dir.join("artifacts");
    let _ = std::fs::create_dir_all(&artifact_dir);

    // Runtime events can burst heavily during streaming/tool execution. Keep a
    // generous bridge buffer so Tauri's emit loop can absorb short frontend or
    // renderer stalls without making text/tool/HYARD updates appear to vanish.
    let (tx, mut rx) = tokio::sync::mpsc::channel(4096);
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

    // Flip the file watcher into "capture" mode. Any file modify events
    // arriving from this point until the matching `end_turn` below get
    // recorded as candidate AI changes. Switching workspaces or starting
    // a new turn would lose any in-flight capture, but that matches the
    // user model — one workspace, one turn at a time.
    file_watcher.start_turn();

    let policy = execution_policy_from_config_with_overrides(&config, &cwd, sandbox_mode, &[]);
    let output = run_routed_turn_observable_with_policy_and_attachments(
        &mut store,
        &mut session,
        &core_proxy,
        &peer_catalog,
        &|name| registry.create(name, config.providers.get(name)),
        Some(registry_dyn.clone()),
        message,
        attachments,
        cwd,
        Some(&artifact_dir),
        Some(&tx),
        cancel.clone(),
        policy,
    )
    .await;

    {
        let state = app.state::<ActiveTurnState>();
        let mut guard = state.cancel.lock().unwrap();
        *guard = None;
    }

    // Drain captured changes and promote each into a FileChange
    // artifact anchored to the just-finished turn. We bind the
    // artifact to the active turn ID so the frontend's
    // `list_ai_file_changes` projection finds them.
    let captured = file_watcher.end_turn();
    if !captured.is_empty() {
        if let Some(turn_id) = latest_turn_id(&store, session.session_id) {
            persist_captured_changes(&mut store, turn_id, &provider, &captured);
        }
    }

    match output {
        Ok(out) => Ok(out.response.unwrap_or_default()),
        Err(e) => Err(format!("turn failed: {}", e)),
    }
}

/// Find the most recent turn for a session — used to anchor file-watcher
/// captured changes after `run_routed_turn_observable` has persisted
/// its turn record.
fn latest_turn_id(store: &StoreHandle, session_id: uuid::Uuid) -> Option<uuid::Uuid> {
    store
        .list_turns(session_id)
        .ok()
        .and_then(|turns| turns.last().map(|t| t.turn_id))
}

/// Build and persist `FileChange` artifacts from file-watcher captures.
/// Provider is stamped onto the metadata so the frontend can show
/// which AI made the change; the path lives on the artifact directly.
fn persist_captured_changes(
    store: &mut StoreHandle,
    turn_id: uuid::Uuid,
    provider: &str,
    changes: &[CapturedChange],
) {
    for change in changes {
        let title = format!("watch → {}", change.path.display());
        let mut artifact =
            Artifact::new(turn_id, switchyard_session::ArtifactType::FileChange, title);
        artifact.summary = Some(format!(
            "Captured file change via workspace watcher ({} bytes before, {} bytes after)",
            change.before.len(),
            change.after.len(),
        ));
        artifact.path = Some(change.path.clone());
        artifact
            .metadata
            .insert("provider".to_string(), serde_json::json!(provider));
        // No upstream tool call to credit, but stamp a synthetic
        // identifier so the diff UI can still discriminate watcher
        // captures from future hook-based captures.
        artifact
            .metadata
            .insert("tool_name".to_string(), serde_json::json!("fs_watcher"));
        artifact
            .metadata
            .insert("before".to_string(), serde_json::json!(change.before));
        artifact
            .metadata
            .insert("after".to_string(), serde_json::json!(change.after));
        if let Err(e) = store.save_artifact(&artifact) {
            eprintln!(
                "[file_watcher] save_artifact for {} failed: {e}",
                change.path.display()
            );
        }
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
async fn update_session_peers(
    workspace_state: tauri::State<'_, WorkspaceState>,
    session_id: String,
    enabled_peers: Vec<String>,
) -> Result<(), String> {
    let (_ws, mut store, _data_dir, _config) = open_current_store(&workspace_state)?;
    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;
    if let Some(mut session) = store
        .load_session(session_uuid)
        .map_err(|e| e.to_string())?
    {
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

/// A FileChange artifact projected into a frontend-friendly shape for
/// the Canvas auto-diff intake. Only artifacts whose metadata carries
/// `before` AND `after` strings flow through here — others (e.g. a
/// PostToolUse that fired without PreToolUse capture) are skipped.
#[derive(Debug, serde::Serialize)]
struct AiFileChangeView {
    /// Stable id so the frontend can dedupe what it has already
    /// surfaced into Canvas tabs.
    artifact_id: String,
    turn_id: String,
    path: Option<String>,
    tool_name: Option<String>,
    provider: Option<String>,
    title: String,
    /// Pre-modification content (Canvas diff baseline).
    before: String,
    /// Post-modification content (current on-disk content captured at
    /// PostToolUse time).
    after: String,
}

/// List AI-driven file changes for a session — one entry per
/// FileChange artifact whose metadata includes both `before` and
/// `after` strings. The frontend polls this on TurnCompleted and pipes
/// each entry into a Canvas tab in diff mode.
///
/// **Path shape**: artifact paths are stored absolute (as the watcher
/// sees them), but the frontend's Canvas tabs are keyed by the
/// workspace-relative form that `list_dir` returns. We rewrite each
/// path to the relative form (falling back to absolute when it lies
/// outside `primary_root`, e.g. an extra_root file) so the tab-match
/// in `surfaceAiFileChange` lands on the existing tab instead of
/// opening a duplicate.
#[tauri::command]
async fn list_ai_file_changes(
    workspace_state: tauri::State<'_, WorkspaceState>,
    session_id: String,
) -> Result<Vec<AiFileChangeView>, String> {
    use switchyard_session::ArtifactType;
    let (ws, store, _data_dir, _config) = open_current_store(&workspace_state)?;
    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    let turns = store
        .list_turns(session_uuid)
        .map_err(|e| format!("list_turns: {}", e))?;

    let primary_norm = lexical_normalize(&ws.primary_root);

    let mut out = Vec::new();
    for turn in &turns {
        let artifacts = match store.list_artifacts(turn.turn_id) {
            Ok(a) => a,
            Err(_) => continue,
        };
        for a in artifacts {
            if !matches!(a.artifact_type, ArtifactType::FileChange) {
                continue;
            }
            let before = match a.metadata.get("before").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue, // no diff baseline; ignore
            };
            let after = match a.metadata.get("after").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let path = a.path.as_ref().map(|p| {
                let norm = lexical_normalize(p);
                norm.strip_prefix(&primary_norm)
                    .ok()
                    .map(|rel| rel.to_string_lossy().to_string())
                    .unwrap_or_else(|| norm.to_string_lossy().to_string())
            });
            out.push(AiFileChangeView {
                artifact_id: a.artifact_id.to_string(),
                turn_id: a.turn_id.to_string(),
                path,
                tool_name: a
                    .metadata
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                provider: a
                    .metadata
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                title: a.title,
                before,
                after,
            });
        }
    }
    Ok(out)
}

#[tauri::command]
async fn list_artifacts(
    workspace_state: tauri::State<'_, WorkspaceState>,
) -> Result<Vec<ArtifactItem>, String> {
    let ws = workspace_state.current()?;
    let artifact_dir = workspace_data_dir(ws.workspace_id).join("artifacts");

    if !artifact_dir.exists() {
        return Ok(Vec::new());
    }

    let mut items = Vec::new();
    let mut dir = tokio::fs::read_dir(&artifact_dir)
        .await
        .map_err(|e| format!("failed to read artifact dir: {}", e))?;

    while let Some(entry) = dir.next_entry().await.map_err(|e| e.to_string())? {
        let metadata = entry.metadata().await.map_err(|e| e.to_string())?;
        let modified = metadata.modified().ok().and_then(|t| {
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
async fn read_artifact(
    workspace_state: tauri::State<'_, WorkspaceState>,
    name: String,
) -> Result<String, String> {
    let ws = workspace_state.current()?;
    let artifact_dir = workspace_data_dir(ws.workspace_id).join("artifacts");

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

/// Slice 1 transitional Tauri commands.
///
/// The frontend's old global `Connect/Disconnect` affordance predates the
/// `(provider, session_id)` keying. These wrappers now require `session_id`,
/// which the existing frontend doesn't pass — those buttons will error at
/// runtime until Slice 3 ships the new Core/Workers panel. Compilation is
/// preserved here so unrelated work keeps moving.
#[tauri::command]
async fn list_active_instances(
    pool: tauri::State<'_, Arc<switchyard_core::InstancePool>>,
    session_id: Option<String>,
) -> Result<Vec<String>, String> {
    use switchyard_provider_api::LiveInstanceRegistry;
    if let Some(sid_str) = session_id {
        let sid =
            uuid::Uuid::parse_str(&sid_str).map_err(|e| format!("invalid session ID: {}", e))?;
        let mut names: Vec<String> = pool
            .list_session(sid)
            .into_iter()
            .map(|m| m.provider)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    } else {
        // No session given — return providers with any live instance anywhere.
        // Slice 3 should replace this with explicit session_id.
        Ok(Vec::new())
    }
}

#[tauri::command]
async fn start_instance(
    pool: tauri::State<'_, Arc<switchyard_core::InstancePool>>,
    workspace_state: tauri::State<'_, WorkspaceState>,
    session_id: String,
    provider: String,
) -> Result<(), String> {
    use switchyard_provider_api::LiveInstanceRegistry;

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;
    let ws = workspace_state.current()?;
    let cwd = ws.primary_root.clone();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let registry = build_registry(&config);

    let provider_impl = registry
        .create(&provider, config.providers.get(&provider))
        .ok_or_else(|| format!("unsupported provider: {}", provider))?;

    if let Some(persistent) = provider_impl.as_persistent() {
        if pool.has_live_instance(&provider, session_uuid) {
            return Ok(()); // Already running for this session.
        }
        let env = config
            .providers
            .get(&provider)
            .map(|c| c.env.clone())
            .unwrap_or_default();
        let inst = persistent
            .start_persistent_instance(cwd.clone(), env)
            .await
            .map_err(|e| format!("failed to start persistent instance: {}", e))?;
        let mut metadata = switchyard_provider_api::InstanceMetadata::new(
            provider.clone(),
            session_uuid,
            None,
            switchyard_provider_api::InstanceKind::Core,
        );
        metadata.state = switchyard_provider_api::InstanceState::Idle;
        pool.register(metadata, inst)
            .map_err(|e| format!("register failed: {}", e))?;
        Ok(())
    } else {
        Err(format!(
            "provider {} does not support persistence",
            provider
        ))
    }
}

#[tauri::command]
async fn stop_instance(
    pool: tauri::State<'_, Arc<switchyard_core::InstancePool>>,
    session_id: String,
    provider: String,
) -> Result<(), String> {
    use switchyard_provider_api::LiveInstanceRegistry;

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;
    let instances = pool.list_session(session_uuid);
    let target = instances.iter().find(|m| m.provider == provider);
    match target {
        Some(meta) => {
            pool.terminate(meta.instance_id);
            Ok(())
        }
        None => Err(format!(
            "no active persistent instance for provider {} in session {}",
            provider, session_id
        )),
    }
}

/// Frontend-friendly snapshot of an instance. `state` is a flat string instead
/// of the tagged enum so TS consumers don't need to discriminate variants;
/// `in_flight_turn_id` is hoisted out of the `Busy` variant.
#[derive(serde::Serialize)]
struct InstanceMetadataView {
    instance_id: String,
    provider: String,
    session_id: String,
    label: Option<String>,
    kind: String,
    spawned_at: String,
    state: String,
    in_flight_turn_id: Option<String>,
}

impl From<switchyard_provider_api::InstanceMetadata> for InstanceMetadataView {
    fn from(m: switchyard_provider_api::InstanceMetadata) -> Self {
        use switchyard_provider_api::{InstanceKind, InstanceState};
        let (state, in_flight) = match m.state {
            InstanceState::Spawning => ("spawning", None),
            InstanceState::Idle => ("idle", None),
            InstanceState::Busy { turn_id } => ("busy", Some(turn_id.to_string())),
            InstanceState::Retrying => ("retrying", None),
            InstanceState::Dying => ("dying", None),
            InstanceState::Dead => ("dead", None),
        };
        let kind = match m.kind {
            InstanceKind::Core => "core",
            InstanceKind::Worker => "worker",
        };
        Self {
            instance_id: m.instance_id.to_string(),
            provider: m.provider,
            session_id: m.session_id.to_string(),
            label: m.label,
            kind: kind.to_string(),
            spawned_at: m.spawned_at.to_rfc3339(),
            state: state.to_string(),
            in_flight_turn_id: in_flight,
        }
    }
}

/// Snapshot of all persistent instances bound to a Switchyard session.
/// Replaces the legacy `list_active_instances` for the new Core/Workers UI.
#[tauri::command]
async fn list_session_workers(
    pool: tauri::State<'_, Arc<switchyard_core::InstancePool>>,
    session_id: String,
) -> Result<Vec<InstanceMetadataView>, String> {
    use switchyard_provider_api::LiveInstanceRegistry;
    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;
    Ok(pool
        .list_session(session_uuid)
        .into_iter()
        .map(Into::into)
        .collect())
}

/// Terminate every Core-kind instance bound to this session. The next
/// `run_turn` will lazily respawn (see the pre-spawn check at the top of
/// `run_turn`). Worker instances are unaffected — they only get cleaned up
/// when the session is deleted.
#[tauri::command]
async fn reset_core(
    app: tauri::AppHandle,
    pool: tauri::State<'_, Arc<switchyard_core::InstancePool>>,
    workspace_state: tauri::State<'_, WorkspaceState>,
    session_id: String,
) -> Result<(), String> {
    use switchyard_provider_api::{InstanceKind, LiveInstanceRegistry};
    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    // Reset is the user explicitly saying "give me a clean slate" — drop any
    // resume tokens so the next `run_turn` does NOT resume the prior CLI
    // thread. Keeping the token would silently undo the reset.
    let ws = workspace_state.current()?;
    let config = SwitchyardConfig::resolve(&ws.primary_root).unwrap_or_default();
    if let Ok((mut store, _data_dir)) = open_workspace_store(&ws, &config)
        && let Ok(Some(mut session)) = store.load_session(session_uuid)
    {
        let keys: Vec<String> = session
            .native_bindings
            .keys()
            .filter(|k| k.ends_with("_resume_token"))
            .cloned()
            .collect();
        let mut changed = false;
        for k in keys {
            session.native_bindings.remove(&k);
            changed = true;
        }
        if changed {
            let _ = store.save_session(&session);
        }
    }

    let cores: Vec<switchyard_provider_api::InstanceMetadata> = pool
        .list_session(session_uuid)
        .into_iter()
        .filter(|m| matches!(m.kind, InstanceKind::Core))
        .collect();
    for meta in cores {
        let instance_id = meta.instance_id;
        let provider = meta.provider.clone();
        let label = meta.label.clone();
        pool.terminate(instance_id);
        // Push out a WorkerTerminated so the frontend removes the Core row
        // immediately without waiting for a list_session_workers refresh.
        let _ = app.emit(
            "runtime_event",
            switchyard_core::RuntimeEvent::WorkerTerminated {
                session_id: session_uuid,
                instance_id,
                provider,
                label,
                reason: "core_reset".to_string(),
            },
        );
    }
    Ok(())
}

/// Rewind canonical history to the point immediately before `turn_id` and
/// position any live Core instance to receive the new (edited) message on
/// a forked thread. The frontend follows this call with a regular
/// `run_turn` carrying the edited text.
///
/// Two paths depending on Core capability:
/// - **Warm fork** (Codex via `thread/fork`): the daemon stays alive, its
///   internal thread rewinds server-side, the next `send_message` lands
///   on the same warm process — no cold-start cost.
/// - **Cold rewind** (Claude, Antigravity, Gemini): we terminate the live
///   instance and drop the cached resume token; the next `run_turn`
///   respawns from scratch. The user's prior conversation context is
///   reconstructed from the canonical store via the Context Composer
///   rather than from the CLI's in-memory state.
#[tauri::command]
async fn edit_and_resend_last_turn(
    app: tauri::AppHandle,
    pool: tauri::State<'_, Arc<switchyard_core::InstancePool>>,
    workspace_state: tauri::State<'_, WorkspaceState>,
    session_id: String,
    turn_id: String,
) -> Result<(), String> {
    use switchyard_provider_api::{InstanceKind, LiveInstanceRegistry};

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;
    let turn_uuid =
        uuid::Uuid::parse_str(&turn_id).map_err(|e| format!("invalid turn ID: {}", e))?;

    let (_ws, mut store, _data_dir, _config) = open_current_store(&workspace_state)?;

    // Compute the user-message turn_index BEFORE rewinding the store: count
    // how many user-origin turns precede the target. This is the index we
    // hand to a warm fork (Codex `thread/fork {turnIndex}` semantics) so the
    // daemon discards from this user message forward.
    let turn_index: u32 = {
        let turns = store
            .list_turns(session_uuid)
            .map_err(|e| format!("list_turns: {}", e))?;
        let mut idx = 0u32;
        for t in &turns {
            if t.turn_id == turn_uuid {
                break;
            }
            if matches!(t.origin, switchyard_session::TurnOrigin::User) {
                idx = idx.saturating_add(1);
            }
        }
        idx
    };

    // 1. Try warm fork on the Core live instance, if one exists.
    let warm_forked = try_warm_fork(&pool, session_uuid, turn_index).await;

    // 2. Always rewind canonical history. The selected turn and everything
    //    later (assistant response, delegations, system feedback) is wiped.
    store
        .delete_turn_tail(turn_uuid)
        .map_err(|e| format!("delete turn tail: {}", e))?;

    // 3. Update or drop the resume token depending on which path won.
    //    Warm fork: persist the NEW thread_id so a future restart resumes
    //    the forked thread. Cold rewind: drop the token entirely so the
    //    next spawn lands on a fresh thread.
    if let Ok(Some(mut session)) = store.load_session(session_uuid) {
        let mut changed = false;
        match &warm_forked {
            Some(outcome) => {
                let key = format!("{}_resume_token", outcome.provider);
                match &outcome.new_resume_token {
                    Some(token) => {
                        session.native_bindings.insert(key, token.clone());
                    }
                    None => {
                        session.native_bindings.remove(&key);
                    }
                }
                changed = true;
            }
            None => {
                let keys: Vec<String> = session
                    .native_bindings
                    .keys()
                    .filter(|k| k.ends_with("_resume_token"))
                    .cloned()
                    .collect();
                for k in keys {
                    session.native_bindings.remove(&k);
                    changed = true;
                }
            }
        }
        if changed {
            let _ = store.save_session(&session);
        }
    }

    // 4. Cold path only: terminate any live Core — its in-memory thread
    //    state no longer matches the canonical store. Workers are left
    //    alone (the user may still want their results).
    if warm_forked.is_none() {
        let cores: Vec<switchyard_provider_api::InstanceMetadata> = pool
            .list_session(session_uuid)
            .into_iter()
            .filter(|m| matches!(m.kind, InstanceKind::Core))
            .collect();
        for meta in cores {
            let instance_id = meta.instance_id;
            let provider = meta.provider.clone();
            let label = meta.label.clone();
            pool.terminate(instance_id);
            let _ = app.emit(
                "runtime_event",
                switchyard_core::RuntimeEvent::WorkerTerminated {
                    session_id: session_uuid,
                    instance_id,
                    provider,
                    label,
                    reason: "edit_rewind".to_string(),
                },
            );
        }
    }

    Ok(())
}

/// Result of a successful warm-fork attempt — used by the caller to decide
/// whether to skip terminate and which resume token to persist.
struct WarmForkOutcome {
    provider: String,
    new_resume_token: Option<String>,
}

/// Attempt a warm fork on the session's Core live instance. Returns `Some`
/// when the instance accepted the rewind; `None` when there's no Core, the
/// Core is busy, or the instance doesn't support `rewind_to` (we'll fall
/// back to the cold path). Errors during rewind also collapse to `None` so
/// the user always gets a working edit.
async fn try_warm_fork(
    pool: &Arc<switchyard_core::InstancePool>,
    session_id: uuid::Uuid,
    turn_index: u32,
) -> Option<WarmForkOutcome> {
    use switchyard_provider_api::{InstanceKind, LiveInstanceRegistry};

    let cores: Vec<switchyard_provider_api::InstanceMetadata> = pool
        .list_session(session_id)
        .into_iter()
        .filter(|m| matches!(m.kind, InstanceKind::Core))
        .filter(|m| matches!(m.state, switchyard_provider_api::InstanceState::Idle))
        .collect();
    let core = cores.into_iter().next()?;
    let inst_lock = pool.checkout_by_id(core.instance_id)?;
    let mut inst = inst_lock.lock().await;
    let rewind_result = inst.rewind_to(turn_index).await;
    let new_token = inst.resume_token();
    drop(inst);
    pool.release(core.instance_id);
    match rewind_result {
        Ok(()) => Some(WarmForkOutcome {
            provider: core.provider,
            new_resume_token: new_token,
        }),
        Err(_) => None,
    }
}

#[tauri::command]
async fn delete_session(
    workspace_state: tauri::State<'_, WorkspaceState>,
    session_id: String,
) -> Result<(), String> {
    let (_ws, mut store, _data_dir, _config) = open_current_store(&workspace_state)?;
    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;
    store
        .delete_session(session_uuid)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn rename_session(
    workspace_state: tauri::State<'_, WorkspaceState>,
    session_id: String,
    name: String,
) -> Result<(), String> {
    let (_ws, mut store, _data_dir, _config) = open_current_store(&workspace_state)?;
    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;
    if let Some(mut session) = store
        .load_session(session_uuid)
        .map_err(|e| e.to_string())?
    {
        session.name = Some(name);
        store.save_session(&session).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn update_session_summary(
    workspace_state: tauri::State<'_, WorkspaceState>,
    session_id: String,
    summary: Option<String>,
) -> Result<(), String> {
    let (_ws, mut store, _data_dir, _config) = open_current_store(&workspace_state)?;
    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;
    if let Some(mut session) = store
        .load_session(session_uuid)
        .map_err(|e| e.to_string())?
    {
        session.summary = summary;
        store.save_session(&session).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn update_session_checklist(
    workspace_state: tauri::State<'_, WorkspaceState>,
    session_id: String,
    checklist_json: String,
) -> Result<(), String> {
    let (_ws, mut store, _data_dir, _config) = open_current_store(&workspace_state)?;
    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;
    if let Some(mut session) = store
        .load_session(session_uuid)
        .map_err(|e| e.to_string())?
    {
        session
            .native_bindings
            .insert("checklist".to_string(), checklist_json);
        store.save_session(&session).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn main() {
    let workspace_state =
        WorkspaceState::load_or_bootstrap().expect("failed to initialise workspace state");

    // Stand up the workspace file watcher before the Tauri runtime
    // starts so the eager-scan thread can begin populating the
    // baseline while the GUI window mounts. If no workspace exists
    // yet (shouldn't happen after bootstrap), the watcher just sits
    // idle until set_current_workspace fires.
    let file_watcher = FileWatcherState::new();
    if let Ok(ws) = workspace_state.current() {
        if let Err(e) = file_watcher.watch_workspace(&ws) {
            eprintln!("[file_watcher] initial watch failed: {e}");
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(ActiveTurnState {
            cancel: std::sync::Mutex::new(None),
        })
        .manage(Arc::new(switchyard_core::InstancePool::new()))
        .manage(workspace_state)
        .manage(file_watcher)
        .manage(pty::PtyState::new())
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
            list_ai_file_changes,
            list_active_instances,
            start_instance,
            stop_instance,
            list_session_workers,
            reset_core,
            edit_and_resend_last_turn,
            delete_session,
            rename_session,
            update_session_summary,
            update_session_checklist,
            // Workspace CRUD
            list_workspaces,
            get_current_workspace,
            open_external_terminal,
            set_current_workspace,
            create_workspace,
            update_workspace,
            delete_workspace,
            // Filesystem (workspace-scoped)
            read_file,
            write_file,
            list_dir,
            // Hook installer surface
            hook_install,
            hook_uninstall,
            hook_status,
            // Source Control (git)
            git_is_repo,
            git_status,
            git_file_diff,
            git_stage,
            git_unstage,
            git_discard,
            git_commit,
            git_init,
            // Embedded terminal (real PTY)
            pty::pty_create,
            pty::pty_write,
            pty::pty_resize,
            pty::pty_close,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_session_name_takes_first_line() {
        let n = derive_session_name("refactor auth\nmake it cleaner");
        assert_eq!(n, "refactor auth");
    }

    #[test]
    fn derive_session_name_strips_leading_punctuation() {
        let n = derive_session_name("# Refactor the auth flow please");
        assert_eq!(n, "Refactor the auth flow please");
    }

    #[test]
    fn derive_session_name_caps_at_eight_words() {
        let n = derive_session_name("one two three four five six seven eight nine ten");
        assert_eq!(n, "one two three four five six seven eight");
    }

    #[test]
    fn derive_session_name_truncates_long_lines_with_ellipsis() {
        // 90-char single "word" to force the char cap.
        let long = "x".repeat(90);
        let n = derive_session_name(&long);
        assert!(n.ends_with('…'));
        assert!(n.chars().count() <= 60);
    }

    #[test]
    fn derive_session_name_returns_empty_for_blank_input() {
        assert_eq!(derive_session_name(""), "");
        assert_eq!(derive_session_name("   \n\n  "), "");
        assert_eq!(derive_session_name("###"), "");
    }

    #[test]
    fn lexical_normalize_collapses_dots_and_parent_refs() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/./c/../d")),
            PathBuf::from("/a/b/d")
        );
        assert_eq!(
            lexical_normalize(Path::new("/a/b/c")),
            PathBuf::from("/a/b/c")
        );
    }

    #[test]
    fn resolve_workspace_path_accepts_files_under_primary_root() {
        // Use a real temp dir so the components stay realistic across
        // platforms (Windows drive letters etc.).
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("src").join("main.rs");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "fn main() {}").unwrap();

        let ws = Workspace::new(tmp.path().to_path_buf());
        // Relative form — what the Files tree sends back.
        let resolved = resolve_workspace_path(&ws, "src/main.rs", None).unwrap();
        assert_eq!(resolved, tmp.path().join("src").join("main.rs"));
        // Absolute form — same file, different shape.
        let resolved_abs =
            resolve_workspace_path(&ws, nested.to_string_lossy().as_ref(), None).unwrap();
        assert_eq!(resolved_abs, nested);
    }

    #[test]
    fn resolve_workspace_path_rejects_traversal_outside_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path().to_path_buf());
        // `..` escape from the workspace must be refused even when the
        // canonical resolution would silently follow it.
        let err = resolve_workspace_path(&ws, "../escape.rs", None).unwrap_err();
        assert!(err.contains("outside workspace roots"), "got: {err}");
    }

    #[test]
    fn resolve_workspace_path_honours_extra_roots() {
        let tmp_primary = tempfile::tempdir().unwrap();
        let tmp_extra = tempfile::tempdir().unwrap();
        let mut ws = Workspace::new(tmp_primary.path().to_path_buf());
        ws.extra_roots.push(tmp_extra.path().to_path_buf());

        let extra_file = tmp_extra.path().join("notes.md");
        std::fs::write(&extra_file, "hi").unwrap();
        let resolved =
            resolve_workspace_path(&ws, extra_file.to_string_lossy().as_ref(), None).unwrap();
        assert_eq!(resolved, extra_file);
    }

    #[test]
    fn resolve_workspace_path_accepts_git_repo_paths_outside_workspace() {
        // The SourceControl panel scenario: workspace primary_root is
        // a SUBDIRECTORY of a larger git repo. Files in sibling subdirs
        // are outside the workspace but inside the repo — the function
        // must accept them when given `Some(repo_root)`.
        let repo = tempfile::tempdir().unwrap();
        let workspace_root = repo.path().join("sub").join("project");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let sibling_file = repo.path().join("other").join("file.rs");
        std::fs::create_dir_all(sibling_file.parent().unwrap()).unwrap();
        std::fs::write(&sibling_file, "fn s() {}").unwrap();

        let ws = Workspace::new(workspace_root);
        // Without the repo-root extension, this is rejected.
        assert!(
            resolve_workspace_path(&ws, sibling_file.to_string_lossy().as_ref(), None,).is_err()
        );
        // With the repo-root extension, the same call succeeds.
        let resolved = resolve_workspace_path(
            &ws,
            sibling_file.to_string_lossy().as_ref(),
            Some(repo.path()),
        )
        .unwrap();
        assert_eq!(resolved, sibling_file);
    }

    #[test]
    fn resolve_workspace_path_still_rejects_paths_outside_repo() {
        // Even with a repo root, paths outside BOTH the workspace AND
        // the repo are refused — defense in depth.
        let repo = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let workspace_root = repo.path().join("sub");
        std::fs::create_dir(&workspace_root).unwrap();
        let ws = Workspace::new(workspace_root);
        let outside = elsewhere.path().join("hostile.rs");
        std::fs::write(&outside, "x").unwrap();
        let err =
            resolve_workspace_path(&ws, outside.to_string_lossy().as_ref(), Some(repo.path()))
                .unwrap_err();
        assert!(err.contains("outside workspace roots"), "got: {err}");
    }
}
