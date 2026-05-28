//! `switchyard host hook` — Switchyard's bridge to CLI provider hook systems.
//!
//! Three modes:
//!
//! 1. `fire <provider> <event>` — invoked by Codex/Claude at runtime when a
//!    hook fires. Reads the hook JSON payload from stdin, looks up the
//!    Switchyard session via `$SWITCHYARD_SESSION_ID`, and appends an
//!    [`Event`] to the canonical store so the consolidated session view
//!    reflects what the CLI is doing internally. Exit 0 — never blocks the
//!    underlying CLI. Policy-based gating (PreToolUse approve/deny) is a
//!    future enhancement; v1 is observation-only.
//!
//! 2. `install --provider <codex|claude|all>` — writes Switchyard hook
//!    entries to the provider's official hook config file:
//!    - Codex: `~/.codex/config.toml` (TOML, `[[hooks.<EventName>]]` array)
//!    - Claude: `~/.claude/hooks.json` (JSON, `hooks.<EventName>: [...]`)
//!      Idempotent: existing user-managed entries are preserved; only entries
//!      flagged `switchyard_managed = true` are touched.
//!
//! 3. `uninstall --provider <codex|claude|all>` — inverse of install.
//!
//! 4. `status` — reports whether Switchyard hooks are present for each
//!    provider plus a machine-readable summary.
//!
//! ## Why observation-only in v1
//!
//! Both Codex and Claude's PreToolUse hooks support blocking decisions
//! (exit non-zero or emit a JSON `decision: "block"` from the hook). The
//! Switchyard policy substrate to make those decisions (allowed paths,
//! write_access, redact patterns) lives in `ExecutionPolicy` but isn't
//! plumbed to the hook handler yet. Wiring that is a separate slice — for
//! now we just record what the CLI does so the canonical session is
//! complete.

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use switchyard_config::SwitchyardConfig;
use switchyard_session::{Event, EventType};
use switchyard_store::{EventLog, SessionRepository, StoreHandle, TurnRepository};
use uuid::Uuid;

/// Env var Switchyard sets on spawned CLI processes so a hook process can
/// discover which Switchyard session it belongs to. Matches the install
/// templates' command line.
pub const ENV_SESSION_ID: &str = "SWITCHYARD_SESSION_ID";

/// Marker field embedded in hook entries we wrote, so uninstall can find
/// and remove only Switchyard's entries without disturbing user-added hooks.
pub const MANAGED_MARKER: &str = "switchyard_managed";

/// Outcome of a `fire` call, returned as JSON on stdout for transparency.
#[derive(Debug, Serialize)]
struct FireOutcome {
    ok: bool,
    provider: String,
    event: String,
    session_id: Option<Uuid>,
    turn_id: Option<Uuid>,
    event_id: Option<Uuid>,
    note: Option<String>,
}

/// JSON document we serialize for the `status` action.
#[derive(Debug, Serialize)]
pub struct HookStatus {
    pub codex_config_path: PathBuf,
    pub codex_installed_events: Vec<String>,
    pub claude_config_path: PathBuf,
    pub claude_installed_events: Vec<String>,
}

/// Events we install hooks for. Both Codex and Claude use the same names.
/// SessionStart is intentionally last so failures during install surface on
/// the most-tested event first.
pub const INSTALLED_EVENTS: &[&str] = &[
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "SessionStart",
];

/// Top-level dispatcher for the `host hook` subcommand. Returns a
/// process exit code so the caller can `process::exit(code)`.
pub async fn run(action: HookAction) -> i32 {
    match action {
        HookAction::Fire { provider, event } => fire(&provider, &event).await,
        HookAction::Install { provider } => install(&provider),
        HookAction::Uninstall { provider } => uninstall(&provider),
        HookAction::Status => status(),
    }
}

/// Subcommand variants. Wired into the clap enum in main.rs.
#[derive(Debug, Clone)]
pub enum HookAction {
    Fire { provider: String, event: String },
    Install { provider: String },
    Uninstall { provider: String },
    Status,
}

// ---------------------------------------------------------------------------
// Fire — runtime hook handler
// ---------------------------------------------------------------------------

async fn fire(provider: &str, event: &str) -> i32 {
    // Best-effort: read whatever the CLI gave us on stdin. Some hook events
    // pass no payload; an empty stdin is fine.
    let mut payload_buf = String::new();
    let _ = io::stdin().read_to_string(&mut payload_buf);
    let payload: serde_json::Value = if payload_buf.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&payload_buf)
            .unwrap_or_else(|_| serde_json::json!({ "raw": payload_buf }))
    };

    let session_id = std::env::var(ENV_SESSION_ID)
        .ok()
        .and_then(|s| Uuid::parse_str(&s).ok());

    // Resolve config + store relative to the cwd the CLI was spawned in.
    // Hook processes inherit cwd from the CLI which inherits it from
    // Switchyard's spawn call, so this lands on the right project.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let store_open = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd));

    let outcome = match (session_id, store_open) {
        (Some(sid), Ok(mut store)) => append_hook_event(&mut store, sid, provider, event, payload),
        (None, _) => FireOutcome {
            ok: false,
            provider: provider.to_string(),
            event: event.to_string(),
            session_id: None,
            turn_id: None,
            event_id: None,
            note: Some(format!("{ENV_SESSION_ID} not set; hook recorded nothing")),
        },
        (Some(_), Err(e)) => FireOutcome {
            ok: false,
            provider: provider.to_string(),
            event: event.to_string(),
            session_id,
            turn_id: None,
            event_id: None,
            note: Some(format!("failed to open canonical store: {e}")),
        },
    };

    // The CLI may pipe our stdout into its own log. Emit a single-line JSON
    // summary so it's grep-friendly without being noisy.
    println!(
        "{}",
        serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string())
    );

    // Exit 0 unconditionally — failing the hook would block the CLI's turn,
    // and v1's contract is "never break the host CLI". Reporting failure via
    // the JSON outcome is enough.
    0
}

fn append_hook_event(
    store: &mut StoreHandle,
    session_id: Uuid,
    provider: &str,
    event: &str,
    payload: serde_json::Value,
) -> FireOutcome {
    // Hook events bind to the session's currently-active turn when one is
    // running; otherwise the latest persisted turn so the event still has a
    // home. SessionStart fires *before* the first turn lands in the store
    // (Switchyard's pre-spawn block runs before the orchestrator persists
    // the turn record), so we tolerate "no turn at all" and skip persisting
    // rather than fail the hook — losing a SessionStart event is far less
    // costly than failing the CLI's spawn.
    let session_loaded = store.load_session(session_id).ok().flatten();
    let turn_id = session_loaded
        .as_ref()
        .and_then(|s| s.active_turn_id)
        .or_else(|| {
            store
                .list_turns(session_id)
                .ok()
                .and_then(|turns| turns.last().map(|t| t.turn_id))
        });

    let Some(turn_id) = turn_id else {
        return FireOutcome {
            ok: true,
            provider: provider.to_string(),
            event: event.to_string(),
            session_id: Some(session_id),
            turn_id: None,
            event_id: None,
            note: Some(format!(
                "no turn yet for session — dropping hook {event} (typical for SessionStart before the first turn)"
            )),
        };
    };

    // (File-change capture used to happen here via PreToolUse → on-disk
    // read → native_bindings stash → PostToolUse pair. That path was
    // replaced by the GUI's workspace file watcher — see
    // `switchyard-gui/src/file_watcher.rs`. The hook still records the
    // raw event below so the canonical session view remains complete.)

    let event_obj = Event::new(
        turn_id,
        EventType::ItemUpdated,
        provider,
        serde_json::json!({
            "item_type": "cli_hook",
            "hook_event": event,
            "received_at": Utc::now().to_rfc3339(),
            "payload": payload.clone(),
        }),
    );
    let event_id = event_obj.event_id;

    if let Err(e) = store.append_event(&event_obj) {
        return FireOutcome {
            ok: false,
            provider: provider.to_string(),
            event: event.to_string(),
            session_id: Some(session_id),
            turn_id: Some(turn_id),
            event_id: None,
            note: Some(format!("append_event failed: {e}")),
        };
    }

    // (PostToolUse-driven artifact promotion was removed alongside the
    // PreToolUse capture above. The file watcher creates FileChange
    // artifacts directly from filesystem observation now.)

    FireOutcome {
        ok: true,
        provider: provider.to_string(),
        event: event.to_string(),
        session_id: Some(session_id),
        turn_id: Some(turn_id),
        event_id: Some(event_id),
        note: None,
    }
}

// ---------------------------------------------------------------------------
// Install / Uninstall
// ---------------------------------------------------------------------------

fn install(provider: &str) -> i32 {
    let providers = expand_provider_selector(provider);
    if providers.is_empty() {
        eprintln!("unknown provider '{provider}'; use codex, claude, or all");
        return 1;
    }
    let mut had_error = false;
    for p in &providers {
        match p.as_str() {
            "codex" => {
                if let Err(e) = install_codex() {
                    eprintln!("install codex hooks failed: {e}");
                    had_error = true;
                } else {
                    println!("installed codex hooks at {}", codex_hooks_path().display());
                }
            }
            "claude" => {
                if let Err(e) = install_claude() {
                    eprintln!("install claude hooks failed: {e}");
                    had_error = true;
                } else {
                    println!(
                        "installed claude hooks at {}",
                        claude_hooks_path().display()
                    );
                }
            }
            other => {
                eprintln!("hook install for '{other}' not supported");
                had_error = true;
            }
        }
    }
    if had_error { 1 } else { 0 }
}

fn uninstall(provider: &str) -> i32 {
    let providers = expand_provider_selector(provider);
    if providers.is_empty() {
        eprintln!("unknown provider '{provider}'; use codex, claude, or all");
        return 1;
    }
    let mut had_error = false;
    for p in &providers {
        match p.as_str() {
            "codex" => {
                if let Err(e) = uninstall_codex() {
                    eprintln!("uninstall codex hooks failed: {e}");
                    had_error = true;
                } else {
                    println!(
                        "removed switchyard hooks from {}",
                        codex_hooks_path().display()
                    );
                }
            }
            "claude" => {
                if let Err(e) = uninstall_claude() {
                    eprintln!("uninstall claude hooks failed: {e}");
                    had_error = true;
                } else {
                    println!(
                        "removed switchyard hooks from {}",
                        claude_hooks_path().display()
                    );
                }
            }
            other => {
                eprintln!("hook uninstall for '{other}' not supported");
                had_error = true;
            }
        }
    }
    if had_error { 1 } else { 0 }
}

fn status() -> i32 {
    let codex_path = codex_hooks_path();
    let claude_path = claude_hooks_path();
    let codex_events = read_codex_installed_events().unwrap_or_default();
    let claude_events = read_claude_installed_events().unwrap_or_default();
    let report = HookStatus {
        codex_config_path: codex_path,
        codex_installed_events: codex_events,
        claude_config_path: claude_path,
        claude_installed_events: claude_events,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
    );
    0
}

fn expand_provider_selector(provider: &str) -> Vec<String> {
    match provider.to_ascii_lowercase().as_str() {
        "all" => vec!["codex".to_string(), "claude".to_string()],
        "codex" | "claude" => vec![provider.to_ascii_lowercase()],
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Codex hooks (TOML)
// ---------------------------------------------------------------------------

fn codex_hooks_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
        .join("config.toml")
}

fn install_codex() -> io::Result<()> {
    let path = codex_hooks_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut root: toml::Value = if existing.trim().is_empty() {
        toml::Value::Table(Default::default())
    } else {
        existing.parse::<toml::Value>().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("parse {}: {e}", path.display()),
            )
        })?
    };

    // Codex's hook format (verified against codex/docs/hooks): an array of
    // tables under `hooks.<EventName>` with `type = "command"` + `command`.
    let exe_path = current_exe_display();
    let root_table = root.as_table_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "config.toml root is not a table",
        )
    })?;
    let hooks_table = root_table
        .entry("hooks")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "`hooks` is not a table"))?;

    for event in INSTALLED_EVENTS {
        let entries = hooks_table
            .entry(event.to_string())
            .or_insert_with(|| toml::Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("hooks.{event} is not an array"),
                )
            })?;

        // Drop any prior Switchyard entry so install is idempotent.
        entries.retain(|v| {
            v.as_table()
                .and_then(|t| t.get(MANAGED_MARKER))
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
        entry.insert(MANAGED_MARKER.into(), toml::Value::Boolean(true));
        entries.push(toml::Value::Table(entry));
    }

    let serialized = toml::to_string_pretty(&root)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialize toml: {e}")))?;
    std::fs::write(&path, serialized)?;
    Ok(())
}

fn uninstall_codex() -> io::Result<()> {
    let path = codex_hooks_path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(()); // nothing to remove
    };
    let mut root: toml::Value = existing.parse::<toml::Value>().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("parse {}: {e}", path.display()),
        )
    })?;
    let mut changed = false;
    if let Some(root_table) = root.as_table_mut()
        && let Some(hooks_table) = root_table.get_mut("hooks").and_then(|h| h.as_table_mut())
    {
        for (_event_name, entries) in hooks_table.iter_mut() {
            if let Some(arr) = entries.as_array_mut() {
                let before = arr.len();
                arr.retain(|v: &toml::Value| {
                    v.as_table()
                        .and_then(|t| t.get(MANAGED_MARKER))
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
            io::Error::new(io::ErrorKind::InvalidData, format!("serialize toml: {e}"))
        })?;
        std::fs::write(&path, serialized)?;
    }
    Ok(())
}

fn read_codex_installed_events() -> io::Result<Vec<String>> {
    let path = codex_hooks_path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let root: toml::Value = existing.parse::<toml::Value>().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("parse {}: {e}", path.display()),
        )
    })?;
    let mut events = Vec::new();
    if let Some(hooks_table) = root.get("hooks").and_then(|h| h.as_table()) {
        for (event_name, entries) in hooks_table {
            if let Some(arr) = entries.as_array() {
                let has_managed = arr.iter().any(|v| {
                    v.as_table()
                        .and_then(|t| t.get(MANAGED_MARKER))
                        .and_then(|m| m.as_bool())
                        == Some(true)
                });
                if has_managed {
                    events.push(event_name.clone());
                }
            }
        }
    }
    events.sort();
    Ok(events)
}

// ---------------------------------------------------------------------------
// Claude hooks (JSON)
// ---------------------------------------------------------------------------

fn claude_hooks_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("hooks.json")
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct ClaudeHooksFile {
    #[serde(default)]
    hooks: HashMap<String, Vec<serde_json::Value>>,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

fn install_claude() -> io::Result<()> {
    let path = claude_hooks_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file: ClaudeHooksFile = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("parse {}: {e}", path.display()),
            )
        })?,
        _ => ClaudeHooksFile::default(),
    };

    let exe_path = current_exe_display();
    for event in INSTALLED_EVENTS {
        let entries = file.hooks.entry((*event).to_string()).or_default();
        entries.retain(|v| {
            v.as_object()
                .and_then(|o| o.get(MANAGED_MARKER))
                .and_then(|m| m.as_bool())
                != Some(true)
        });
        entries.push(serde_json::json!({
            "type": "command",
            "command": format!("{exe_path} host hook fire --provider claude --event {event}"),
            "description": "switchyard:managed",
            MANAGED_MARKER: true,
        }));
    }

    let serialized = serde_json::to_string_pretty(&file)?;
    std::fs::write(&path, serialized)?;
    Ok(())
}

fn uninstall_claude() -> io::Result<()> {
    let path = claude_hooks_path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    if existing.trim().is_empty() {
        return Ok(());
    }
    let mut file: ClaudeHooksFile = serde_json::from_str(&existing).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("parse {}: {e}", path.display()),
        )
    })?;
    let mut changed = false;
    for entries in file.hooks.values_mut() {
        let before = entries.len();
        entries.retain(|v| {
            v.as_object()
                .and_then(|o| o.get(MANAGED_MARKER))
                .and_then(|m| m.as_bool())
                != Some(true)
        });
        if entries.len() != before {
            changed = true;
        }
    }
    file.hooks.retain(|_, v| !v.is_empty());
    if changed {
        let serialized = serde_json::to_string_pretty(&file)?;
        std::fs::write(&path, serialized)?;
    }
    Ok(())
}

fn read_claude_installed_events() -> io::Result<Vec<String>> {
    let path = claude_hooks_path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    if existing.trim().is_empty() {
        return Ok(Vec::new());
    }
    let file: ClaudeHooksFile = serde_json::from_str(&existing).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("parse {}: {e}", path.display()),
        )
    })?;
    let mut events = Vec::new();
    for (event_name, entries) in &file.hooks {
        let has_managed = entries.iter().any(|v| {
            v.as_object()
                .and_then(|o| o.get(MANAGED_MARKER))
                .and_then(|m| m.as_bool())
                == Some(true)
        });
        if has_managed {
            events.push(event_name.clone());
        }
    }
    events.sort();
    Ok(events)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Path to the currently-running `switchyard` executable, used as the
/// `command` field of installed hook entries so the hook always invokes
/// the version of Switchyard the user just installed from.
fn current_exe_display() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "switchyard".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_session::Session;
    use switchyard_store::SessionEventRepository;
    use tempfile::TempDir;

    fn isolated_home() -> (TempDir, std::sync::MutexGuard<'static, ()>) {
        // Serialize env mutation so parallel tests don't trample each other.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("HOME", dir.path());
            std::env::set_var("USERPROFILE", dir.path());
        }
        (dir, guard)
    }

    #[test]
    fn append_hook_event_records_event_under_latest_turn() {
        use switchyard_session::{Turn, TurnRole};

        let dir = tempfile::tempdir().unwrap();
        let mut store = StoreHandle::open(
            switchyard_store::StoreBackend::Jsonl,
            dir.path().to_path_buf(),
        )
        .unwrap();

        let session = Session::new("codex".to_string());
        let session_id = session.session_id;
        store.save_session(&session).unwrap();

        // Seed a turn so the hook event has somewhere to anchor.
        let turn = Turn::new(session_id, "codex", TurnRole::Core, "hello");
        let turn_id = turn.turn_id;
        store.append_turn(&turn).unwrap();

        let outcome = append_hook_event(
            &mut store,
            session_id,
            "codex",
            "PreToolUse",
            serde_json::json!({ "tool_name": "ls" }),
        );
        assert!(outcome.ok, "outcome failed: {:?}", outcome.note);
        assert_eq!(outcome.session_id, Some(session_id));
        assert_eq!(outcome.turn_id, Some(turn_id));
        assert!(outcome.event_id.is_some());

        let events = store.list_session_events(session_id).unwrap();
        assert_eq!(events.len(), 1);
        let recorded = &events[0];
        assert_eq!(recorded.event_type, EventType::ItemUpdated);
        assert_eq!(recorded.provider, "codex");
        assert_eq!(
            recorded.payload.get("hook_event").and_then(|v| v.as_str()),
            Some("PreToolUse")
        );
        assert_eq!(
            recorded.payload.get("item_type").and_then(|v| v.as_str()),
            Some("cli_hook")
        );
        assert_eq!(
            recorded
                .payload
                .get("payload")
                .and_then(|p| p.get("tool_name"))
                .and_then(|v| v.as_str()),
            Some("ls")
        );
    }

    #[test]
    fn append_hook_event_drops_event_when_no_turn_exists() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = StoreHandle::open(
            switchyard_store::StoreBackend::Jsonl,
            dir.path().to_path_buf(),
        )
        .unwrap();

        let session = Session::new("codex".to_string());
        let session_id = session.session_id;
        store.save_session(&session).unwrap();
        // Deliberately do NOT append any turn — simulates a SessionStart
        // hook firing before the orchestrator persists the first turn.

        let outcome = append_hook_event(
            &mut store,
            session_id,
            "codex",
            "SessionStart",
            serde_json::json!({}),
        );
        assert!(outcome.ok, "must succeed even with no turn");
        assert!(outcome.event_id.is_none(), "no event persisted");
        assert!(outcome.note.is_some(), "note explains the drop");
    }

    #[test]
    fn install_codex_writes_marker_entries() {
        let (_home, _lock) = isolated_home();
        install_codex().unwrap();
        let events = read_codex_installed_events().unwrap();
        for required in INSTALLED_EVENTS {
            assert!(events.contains(&(*required).to_string()), "{required}");
        }
    }

    #[test]
    fn install_codex_is_idempotent() {
        let (_home, _lock) = isolated_home();
        install_codex().unwrap();
        install_codex().unwrap();
        let raw = std::fs::read_to_string(codex_hooks_path()).unwrap();
        let parsed: toml::Value = raw.parse().unwrap();
        for event in INSTALLED_EVENTS {
            let arr = parsed
                .get("hooks")
                .and_then(|h| h.get(event))
                .and_then(|e| e.as_array())
                .expect(event);
            let managed: Vec<_> = arr
                .iter()
                .filter(|v| {
                    v.as_table()
                        .and_then(|t| t.get(MANAGED_MARKER))
                        .and_then(|m| m.as_bool())
                        == Some(true)
                })
                .collect();
            assert_eq!(managed.len(), 1, "duplicate switchyard entry for {event}");
        }
    }

    #[test]
    fn uninstall_codex_preserves_user_entries() {
        let (_home, _lock) = isolated_home();
        // Seed config with a user-managed entry alongside ours.
        install_codex().unwrap();
        let path = codex_hooks_path();
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut root: toml::Value = raw.parse().unwrap();
        let session_arr = root
            .get_mut("hooks")
            .unwrap()
            .get_mut("SessionStart")
            .unwrap()
            .as_array_mut()
            .unwrap();
        let mut user_entry = toml::value::Table::new();
        user_entry.insert("type".into(), toml::Value::String("command".into()));
        user_entry.insert("command".into(), toml::Value::String("echo hello".into()));
        // Note: no MANAGED_MARKER on this entry.
        session_arr.push(toml::Value::Table(user_entry));
        std::fs::write(&path, toml::to_string_pretty(&root).unwrap()).unwrap();

        uninstall_codex().unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = raw.parse().unwrap();
        let session_arr = parsed
            .get("hooks")
            .and_then(|h| h.get("SessionStart"))
            .and_then(|e| e.as_array())
            .unwrap();
        assert_eq!(session_arr.len(), 1, "user entry must survive uninstall");
        assert_eq!(
            session_arr[0]
                .as_table()
                .and_then(|t| t.get("command"))
                .and_then(|c| c.as_str()),
            Some("echo hello")
        );
    }

    #[test]
    fn install_claude_writes_marker_entries() {
        let (_home, _lock) = isolated_home();
        install_claude().unwrap();
        let events = read_claude_installed_events().unwrap();
        for required in INSTALLED_EVENTS {
            assert!(events.contains(&(*required).to_string()), "{required}");
        }
    }

    #[test]
    fn install_claude_is_idempotent_and_preserves_user_entries() {
        let (_home, _lock) = isolated_home();
        // Pre-seed with a user-defined hook.
        let path = claude_hooks_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
              "hooks": {
                "SessionStart": [
                  { "type": "command", "command": "echo seed" }
                ]
              }
            }"#,
        )
        .unwrap();

        install_claude().unwrap();
        install_claude().unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: ClaudeHooksFile = serde_json::from_str(&raw).unwrap();
        let session_start = parsed.hooks.get("SessionStart").unwrap();
        assert!(session_start.len() >= 2);
        let managed: Vec<_> = session_start
            .iter()
            .filter(|v| {
                v.as_object()
                    .and_then(|o| o.get(MANAGED_MARKER))
                    .and_then(|m| m.as_bool())
                    == Some(true)
            })
            .collect();
        assert_eq!(managed.len(), 1, "idempotent: exactly one managed entry");
        let preserved = session_start
            .iter()
            .find(|v| {
                v.as_object()
                    .and_then(|o| o.get("command"))
                    .and_then(|c| c.as_str())
                    == Some("echo seed")
            })
            .expect("user entry preserved");
        assert!(preserved.as_object().unwrap().get(MANAGED_MARKER).is_none());
    }
}
