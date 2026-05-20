use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use switchyard_config::SwitchyardConfig;
use switchyard_store::{
    ArtifactStore, EventLog, SessionCatalog, SessionEventRepository, SessionRepository,
    StoreBackend, StoreHandle, TurnRepository,
};
use uuid::Uuid;

const DOT_SWITCHYARD_DIR: &str = ".switchyard";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliStoreBackend {
    Jsonl,
    Sqlite,
}

impl From<CliStoreBackend> for StoreBackend {
    fn from(value: CliStoreBackend) -> Self {
        match value {
            CliStoreBackend::Jsonl => StoreBackend::Jsonl,
            CliStoreBackend::Sqlite => StoreBackend::Sqlite,
        }
    }
}

#[derive(Debug, Clone, Args)]
struct StoreSelectorArgs {
    /// Store backend override (otherwise uses configured/default backend).
    #[arg(long, value_enum)]
    backend: Option<CliStoreBackend>,

    /// Store path override. Relative paths resolve from the current project root.
    #[arg(long)]
    path: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct StoreInspectArgs {
    #[command(flatten)]
    selector: StoreSelectorArgs,

    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct StoreListSessionsArgs {
    #[command(flatten)]
    selector: StoreSelectorArgs,

    /// Show only the most recently updated N sessions.
    #[arg(long)]
    limit: Option<usize>,

    /// Human-readable compact table without per-status count columns.
    #[arg(long)]
    short: bool,

    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct StoreCheckArgs {
    #[command(flatten)]
    selector: StoreSelectorArgs,

    /// Limit the integrity check to one session id or unique id prefix.
    #[arg(long)]
    session: Option<String>,

    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct StoreShowArgs {
    #[command(flatten)]
    selector: StoreSelectorArgs,

    /// Show one session by full id or unique id prefix.
    #[arg(value_name = "SESSION", conflicts_with_all = ["session", "latest"])]
    session_ref: Option<String>,

    /// Show one session by full id or unique id prefix.
    #[arg(long, conflicts_with_all = ["session_ref", "latest"])]
    session: Option<String>,

    /// Show the most recently updated session.
    #[arg(long, conflicts_with_all = ["session_ref", "session"])]
    latest: bool,

    /// Include per-event and per-artifact detail in human-readable output.
    #[arg(long, short = 'v')]
    verbose: bool,

    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct StoreMigrateArgs {
    /// Source backend override (defaults to configured/default backend).
    #[arg(long = "from-backend", value_enum)]
    from_backend: Option<CliStoreBackend>,

    /// Source path override. Relative paths resolve from the current project root.
    #[arg(long = "from-path")]
    from_path: Option<PathBuf>,

    /// Limit the migration/verification scope to one session id or unique id prefix.
    #[arg(long)]
    session: Option<String>,

    /// Destination backend.
    #[arg(long = "to-backend", value_enum)]
    to_backend: CliStoreBackend,

    /// Destination path override. Relative paths resolve from the current project root.
    #[arg(long = "to-path")]
    to_path: Option<PathBuf>,

    /// Preview counts and validation outcome without writing target data.
    #[arg(long)]
    dry_run: bool,

    /// Compare source and target as they currently exist without writing target data.
    #[arg(long)]
    verify_only: bool,

    /// Re-open the target store after migration and verify aggregate counts match the source.
    #[arg(long)]
    verify: bool,

    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Subcommand)]
pub enum StoreAction {
    /// Summarize the currently selected store and aggregate object counts.
    #[command(alias = "info")]
    Inspect(StoreInspectArgs),
    /// List stored sessions with per-session counts.
    #[command(alias = "ls")]
    ListSessions(StoreListSessionsArgs),
    /// Validate store consistency and backend-specific integrity checks.
    Check(StoreCheckArgs),
    /// Show one session with turn/event/artifact detail.
    Show(StoreShowArgs),
    /// Copy canonical session/turn/event/artifact data from one store to another.
    Migrate(StoreMigrateArgs),
}

#[derive(Debug, Clone)]
struct ResolvedStoreEndpoint {
    backend: StoreBackend,
    path: PathBuf,
}

impl ResolvedStoreEndpoint {
    fn backend_name(&self) -> &'static str {
        backend_name(self.backend)
    }

    fn path_display(&self) -> String {
        self.path.display().to_string()
    }

    fn open(&self) -> Result<StoreHandle, String> {
        StoreHandle::open(self.backend, self.path.clone()).map_err(|err| {
            format!(
                "open {} store at '{}': {err}",
                self.backend_name(),
                self.path.display()
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
struct StoreCounts {
    sessions: usize,
    turns: usize,
    events: usize,
    artifacts: usize,
}

impl StoreCounts {
    fn is_empty(&self) -> bool {
        self.sessions == 0 && self.turns == 0 && self.events == 0 && self.artifacts == 0
    }
}

#[derive(Debug, Clone)]
struct StoreSnapshot {
    path_exists: bool,
    sessions: Vec<SessionListEntry>,
    sqlite_schema: Option<switchyard_store::SqliteSchemaInfo>,
}

impl StoreSnapshot {
    fn counts(&self) -> StoreCounts {
        let mut counts = StoreCounts {
            sessions: self.sessions.len(),
            ..StoreCounts::default()
        };
        for session in &self.sessions {
            counts.turns += session.turns;
            counts.events += session.events;
            counts.artifacts += session.artifacts;
        }
        counts
    }

    fn active_cores(&self) -> Vec<String> {
        let mut active_cores = BTreeSet::new();
        for session in &self.sessions {
            if let Some(active_core) = &session.active_core {
                active_cores.insert(active_core.clone());
            }
        }
        active_cores.into_iter().collect()
    }

    fn latest_session_updated_at(&self) -> Option<String> {
        self.sessions
            .iter()
            .filter_map(|session| session.updated_at.clone())
            .max()
    }

    fn verification_scope(&self) -> BTreeMap<String, SessionListEntry> {
        self.sessions
            .iter()
            .cloned()
            .map(|session| (session.session_id.clone(), session))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
struct StoreInspectReport {
    backend: String,
    path: String,
    path_exists: bool,
    sessions: usize,
    turns: usize,
    events: usize,
    artifacts: usize,
    active_cores: Vec<String>,
    latest_session_updated_at: Option<String>,
    sqlite_schema_version: Option<i64>,
    sqlite_store_id: Option<String>,
    sqlite_created_at: Option<String>,
    sqlite_migration_versions: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SessionListEntry {
    session_id: String,
    active_core: Option<String>,
    updated_at: Option<String>,
    turns: usize,
    events: usize,
    artifacts: usize,
    completed_turns: usize,
    failed_turns: usize,
    cancelled_turns: usize,
    delegate_turns: usize,
}

#[derive(Debug, Clone, Serialize)]
struct SessionListReport {
    backend: String,
    path: String,
    path_exists: bool,
    total_sessions: usize,
    displayed_sessions: usize,
    limit: Option<usize>,
    sessions: Vec<SessionListEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct StoreCheckReport {
    backend: String,
    path: String,
    path_exists: bool,
    ok: bool,
    counts: StoreCounts,
    issues: Vec<String>,
    warnings: Vec<String>,
    sqlite_schema_version: Option<i64>,
    sqlite_store_id: Option<String>,
    sqlite_integrity_errors: Vec<String>,
    sqlite_foreign_key_issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct StoreShowReport {
    backend: String,
    path: String,
    path_exists: bool,
    selection: SessionSelectionReport,
    session: SessionShowEntry,
    turns: Vec<TurnShowEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct SessionSelectionReport {
    mode: String,
    query: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SessionShowEntry {
    session_id: String,
    created_at: String,
    updated_at: String,
    duration_ms: i64,
    active_core: String,
    enabled_peers: Vec<String>,
    mode: String,
    summary: Option<String>,
    native_bindings: BTreeMap<String, String>,
    counts: StoreCounts,
}

#[derive(Debug, Clone, Serialize)]
struct TurnShowEntry {
    sequence: usize,
    turn_id: String,
    origin: String,
    provider: String,
    role: String,
    status: String,
    started_at: String,
    completed_at: Option<String>,
    duration_ms: Option<i64>,
    delegated_by: Option<String>,
    user_message: String,
    provider_response: Option<String>,
    error_message: Option<String>,
    events: Vec<EventShowEntry>,
    artifacts: Vec<ArtifactShowEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct EventShowEntry {
    event_id: String,
    event_type: String,
    provider: String,
    timestamp: String,
    payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
struct ArtifactShowEntry {
    artifact_id: String,
    artifact_type: String,
    title: String,
    summary: Option<String>,
    path: Option<String>,
    metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct MigrationCounts {
    sessions: usize,
    turns: usize,
    events: usize,
    artifacts: usize,
}

#[derive(Debug, Clone, Serialize)]
struct MigrationVerification {
    matches: bool,
    count_matches: bool,
    session_scope_matches: bool,
    source: StoreCounts,
    target: StoreCounts,
    missing_sessions: Vec<String>,
    unexpected_sessions: Vec<String>,
    mismatched_sessions: Vec<SessionVerificationMismatch>,
}

#[derive(Debug, Clone, Serialize)]
struct SessionVerificationMismatch {
    session_id: String,
    source: SessionListEntry,
    target: SessionListEntry,
}

#[derive(Debug, Clone, Serialize)]
struct MigrationReport {
    mode: String,
    selected_session: Option<String>,
    from_backend: String,
    from_path: String,
    from_path_exists: bool,
    to_backend: String,
    to_path: String,
    to_path_exists_before: bool,
    source_counts: StoreCounts,
    target_counts_before: StoreCounts,
    migrated: MigrationCounts,
    can_apply: bool,
    verified: bool,
    verification: Option<MigrationVerification>,
    warnings: Vec<String>,
}

pub fn run(action: StoreAction, config: &SwitchyardConfig, cwd: &Path) -> Result<(), String> {
    match action {
        StoreAction::Inspect(args) => inspect_store(args, config, cwd),
        StoreAction::ListSessions(args) => list_sessions(args, config, cwd),
        StoreAction::Check(args) => check_store(args, config, cwd),
        StoreAction::Show(args) => show_session(args, config, cwd),
        StoreAction::Migrate(args) => migrate_store(args, config, cwd),
    }
}

fn inspect_store(
    args: StoreInspectArgs,
    config: &SwitchyardConfig,
    cwd: &Path,
) -> Result<(), String> {
    let endpoint = resolve_endpoint(&args.selector, config, cwd);
    let snapshot = load_store_snapshot(&endpoint, None)?;
    let counts = snapshot.counts();
    let sqlite_schema = snapshot.sqlite_schema.as_ref();
    let report = StoreInspectReport {
        backend: endpoint.backend_name().to_string(),
        path: endpoint.path_display(),
        path_exists: snapshot.path_exists,
        sessions: counts.sessions,
        turns: counts.turns,
        events: counts.events,
        artifacts: counts.artifacts,
        active_cores: snapshot.active_cores(),
        latest_session_updated_at: snapshot.latest_session_updated_at(),
        sqlite_schema_version: sqlite_schema.map(|schema| schema.schema_version),
        sqlite_store_id: sqlite_schema.and_then(|schema| schema.store_id.clone()),
        sqlite_created_at: sqlite_schema.and_then(|schema| schema.created_at.clone()),
        sqlite_migration_versions: sqlite_schema
            .map(|schema| {
                schema
                    .migrations
                    .iter()
                    .map(|migration| migration.version)
                    .collect()
            })
            .unwrap_or_default(),
    };

    if args.json {
        print_json(&report)
    } else {
        println!("store backend: {}", report.backend);
        println!("store path: {}", report.path);
        println!("path exists: {}", yes_no(report.path_exists));
        println!("sessions: {}", report.sessions);
        println!("turns: {}", report.turns);
        println!("events: {}", report.events);
        println!("artifacts: {}", report.artifacts);
        println!(
            "active cores: {}",
            if report.active_cores.is_empty() {
                "<none>".to_string()
            } else {
                report.active_cores.join(", ")
            }
        );
        println!(
            "latest session update: {}",
            report
                .latest_session_updated_at
                .as_deref()
                .unwrap_or("<none>")
        );
        if let Some(schema_version) = report.sqlite_schema_version {
            println!("sqlite schema version: {schema_version}");
            println!(
                "sqlite store id: {}",
                report.sqlite_store_id.as_deref().unwrap_or("<unknown>")
            );
            println!(
                "sqlite created_at: {}",
                report.sqlite_created_at.as_deref().unwrap_or("<unknown>")
            );
            let versions = if report.sqlite_migration_versions.is_empty() {
                "<none>".to_string()
            } else {
                report
                    .sqlite_migration_versions
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            println!("sqlite migrations: {versions}");
        }
        Ok(())
    }
}

fn list_sessions(
    args: StoreListSessionsArgs,
    config: &SwitchyardConfig,
    cwd: &Path,
) -> Result<(), String> {
    if args.limit == Some(0) {
        return Err("`--limit` must be greater than 0".to_string());
    }

    let endpoint = resolve_endpoint(&args.selector, config, cwd);
    let snapshot = load_store_snapshot(&endpoint, None)?;
    let total_sessions = snapshot.sessions.len();
    let sessions = limit_session_entries(snapshot.sessions, args.limit);
    let report = SessionListReport {
        backend: endpoint.backend_name().to_string(),
        path: endpoint.path_display(),
        path_exists: snapshot.path_exists,
        total_sessions,
        displayed_sessions: sessions.len(),
        limit: args.limit,
        sessions,
    };

    if args.json {
        print_json(&report)
    } else if report.sessions.is_empty() {
        println!(
            "no sessions found in {} store at {}",
            report.backend, report.path
        );
        Ok(())
    } else {
        println!("store backend: {}", report.backend);
        println!("store path: {}", report.path);
        println!("path exists: {}", yes_no(report.path_exists));
        if let Some(limit) = report.limit
            && report.displayed_sessions < report.total_sessions
        {
            println!(
                "showing {} of {} sessions (limit={limit})",
                report.displayed_sessions, report.total_sessions
            );
        }
        print_session_table(&report.sessions, args.short);
        if !args.short {
            println!(
                "hint: use `switchyard store show <session-id-or-prefix>` to inspect one session, or omit the selector to open the latest session"
            );
            println!(
                "hint: use `switchyard tui --session <session-id-or-prefix>` to resume in the TUI, or `switchyard tui --resume-latest` for the newest session"
            );
        }
        Ok(())
    }
}

fn check_store(args: StoreCheckArgs, config: &SwitchyardConfig, cwd: &Path) -> Result<(), String> {
    let endpoint = resolve_endpoint(&args.selector, config, cwd);
    let path_exists = endpoint.path.exists();
    let mut issues = Vec::new();
    let mut warnings = Vec::new();
    let mut counts = StoreCounts::default();
    let mut sqlite_schema_version = None;
    let mut sqlite_store_id = None;
    let mut sqlite_integrity_errors = Vec::new();
    let mut sqlite_foreign_key_issues = Vec::new();

    if !path_exists {
        issues.push(format!(
            "store path '{}' does not exist",
            endpoint.path.display()
        ));
    } else {
        let store = endpoint.open()?;
        let session_ids = selected_session_ids(&store, args.session.as_deref())?;
        let total_sessions = session_ids.len();
        counts.sessions = session_ids.len();

        if session_ids.is_empty() {
            warnings.push("store is empty".to_string());
        }

        for (index, session_id) in session_ids.into_iter().enumerate() {
            if !args.json {
                println!(
                    "[{}/{}] checking session {}",
                    index + 1,
                    total_sessions,
                    session_id
                );
            }

            let session = store
                .load_session(session_id)
                .map_err(|err| format!("load session '{session_id}': {err}"))?;
            if session.is_none() {
                issues.push(format!(
                    "session '{}' is listed but cannot be loaded",
                    session_id
                ));
            }

            let turns = store
                .list_turns(session_id)
                .map_err(|err| format!("list turns for session '{session_id}': {err}"))?;
            counts.turns += turns.len();
            let turn_ids = turns
                .iter()
                .map(|turn| turn.turn_id)
                .collect::<HashSet<_>>();

            let events = store
                .list_session_events(session_id)
                .map_err(|err| format!("list events for session '{session_id}': {err}"))?;
            counts.events += events.len();
            for event in &events {
                if !turn_ids.contains(&event.turn_id) {
                    issues.push(format!(
                        "session '{}' has event '{}' referencing missing turn '{}'",
                        session_id, event.event_id, event.turn_id
                    ));
                }
            }

            for turn in &turns {
                match store.list_artifacts(turn.turn_id) {
                    Ok(artifacts) => {
                        counts.artifacts += artifacts.len();
                    }
                    Err(err) => issues.push(format!(
                        "failed to list artifacts for turn '{}': {err}",
                        turn.turn_id
                    )),
                }
            }
        }

        if let Some(schema) = store
            .sqlite_schema_info()
            .map_err(|err| format!("inspect sqlite schema '{}': {err}", endpoint.path.display()))?
        {
            sqlite_schema_version = Some(schema.schema_version);
            sqlite_store_id = schema.store_id;
        }
        if let Some(health) = store
            .sqlite_health_info()
            .map_err(|err| format!("inspect sqlite health '{}': {err}", endpoint.path.display()))?
        {
            sqlite_integrity_errors = health.integrity_errors;
            sqlite_foreign_key_issues = health.foreign_key_issues;
            for error in &sqlite_integrity_errors {
                issues.push(format!("sqlite integrity_check: {error}"));
            }
            for issue in &sqlite_foreign_key_issues {
                issues.push(format!("sqlite foreign_key_check: {issue}"));
            }
        }
    }

    let report = StoreCheckReport {
        backend: endpoint.backend_name().to_string(),
        path: endpoint.path_display(),
        path_exists,
        ok: issues.is_empty(),
        counts,
        issues,
        warnings,
        sqlite_schema_version,
        sqlite_store_id,
        sqlite_integrity_errors,
        sqlite_foreign_key_issues,
    };

    if args.json {
        print_json(&report)?;
    } else {
        println!("store backend: {}", report.backend);
        println!("store path: {}", report.path);
        println!("path exists: {}", yes_no(report.path_exists));
        println!("counts: {}", format_counts(&report.counts));
        println!("status: {}", if report.ok { "ok" } else { "failed" });
        if let Some(schema_version) = report.sqlite_schema_version {
            println!("sqlite schema version: {schema_version}");
            println!(
                "sqlite store id: {}",
                report.sqlite_store_id.as_deref().unwrap_or("<unknown>")
            );
        }
        if !report.warnings.is_empty() {
            println!("warnings:");
            for warning in &report.warnings {
                println!("  - {warning}");
            }
        }
        if !report.issues.is_empty() {
            println!("issues:");
            for issue in &report.issues {
                println!("  - {issue}");
            }
        }
    }

    if report.ok {
        Ok(())
    } else {
        Err(format!(
            "store check found {} issue(s)",
            report.issues.len()
        ))
    }
}

fn show_session(args: StoreShowArgs, config: &SwitchyardConfig, cwd: &Path) -> Result<(), String> {
    let endpoint = resolve_endpoint(&args.selector, config, cwd);
    let path_exists = endpoint.path.exists();
    if !path_exists {
        return Err(format!(
            "store path '{}' does not exist",
            endpoint.path.display()
        ));
    }

    let store = endpoint.open()?;
    let (session_id, selection) = resolve_store_show_selection(&store, &args)?;
    let session = store
        .load_session(session_id)
        .map_err(|err| format!("load session '{session_id}': {err}"))?
        .ok_or_else(|| format!("session '{session_id}' not found in selected store"))?;
    let session_duration_ms = session
        .updated_at
        .signed_duration_since(session.created_at)
        .num_milliseconds();
    let turns = store
        .list_turns(session_id)
        .map_err(|err| format!("list turns for session '{session_id}': {err}"))?;
    let mut turn_entries = Vec::new();
    let mut total_events = 0usize;
    let mut total_artifacts = 0usize;

    for (index, turn) in turns.iter().enumerate() {
        let events = store
            .list_events(turn.turn_id)
            .map_err(|err| format!("list events for turn '{}': {err}", turn.turn_id))?;
        let artifacts = store
            .list_artifacts(turn.turn_id)
            .map_err(|err| format!("list artifacts for turn '{}': {err}", turn.turn_id))?;
        total_events += events.len();
        total_artifacts += artifacts.len();

        turn_entries.push(TurnShowEntry {
            sequence: index + 1,
            turn_id: turn.turn_id.to_string(),
            origin: turn.origin.to_string(),
            provider: turn.provider.clone(),
            role: format!("{:?}", turn.role).to_ascii_lowercase(),
            status: turn.status.to_string(),
            started_at: turn.started_at.to_rfc3339(),
            completed_at: turn.completed_at.as_ref().map(|value| value.to_rfc3339()),
            duration_ms: turn.completed_at.as_ref().map(|value| {
                value
                    .signed_duration_since(turn.started_at)
                    .num_milliseconds()
            }),
            delegated_by: turn.delegated_by.clone(),
            user_message: turn.user_message.clone(),
            provider_response: turn.provider_response.clone(),
            error_message: turn.error_message.clone(),
            events: events
                .into_iter()
                .map(|event| EventShowEntry {
                    event_id: event.event_id.to_string(),
                    event_type: event.event_type.to_string(),
                    provider: event.provider,
                    timestamp: event.timestamp.to_rfc3339(),
                    payload: event.payload,
                })
                .collect(),
            artifacts: artifacts
                .into_iter()
                .map(|artifact| ArtifactShowEntry {
                    artifact_id: artifact.artifact_id.to_string(),
                    artifact_type: artifact.artifact_type.to_string(),
                    title: artifact.title,
                    summary: artifact.summary,
                    path: artifact.path.map(|path| path.display().to_string()),
                    metadata: artifact.metadata.into_iter().collect(),
                })
                .collect(),
        });
    }

    let report = StoreShowReport {
        backend: endpoint.backend_name().to_string(),
        path: endpoint.path_display(),
        path_exists,
        selection,
        session: SessionShowEntry {
            session_id: session.session_id.to_string(),
            created_at: session.created_at.to_rfc3339(),
            updated_at: session.updated_at.to_rfc3339(),
            duration_ms: session_duration_ms,
            active_core: session.active_core,
            enabled_peers: session.enabled_peers,
            mode: format!("{:?}", session.mode).to_ascii_lowercase(),
            summary: session.summary,
            native_bindings: session.native_bindings.into_iter().collect(),
            counts: StoreCounts {
                sessions: 1,
                turns: turn_entries.len(),
                events: total_events,
                artifacts: total_artifacts,
            },
        },
        turns: turn_entries,
    };

    if args.json {
        print_json(&report)
    } else {
        println!("store backend: {}", report.backend);
        println!("store path: {}", report.path);
        println!("path exists: {}", yes_no(report.path_exists));
        println!(
            "selected by: {}",
            format_store_show_selection(&report.selection)
        );
        println!("session id: {}", report.session.session_id);
        println!("active core: {}", report.session.active_core);
        println!("mode: {}", report.session.mode);
        println!("created_at: {}", report.session.created_at);
        println!("updated_at: {}", report.session.updated_at);
        println!(
            "duration: {}",
            format_duration_ms(report.session.duration_ms)
        );
        println!("counts: {}", format_counts(&report.session.counts));
        println!(
            "enabled peers: {}",
            join_or_none(&report.session.enabled_peers)
        );
        println!(
            "summary: {}",
            report.session.summary.as_deref().unwrap_or("<none>")
        );
        if report.session.native_bindings.is_empty() {
            println!("native bindings: <none>");
        } else {
            println!("native bindings:");
            for (key, value) in &report.session.native_bindings {
                println!("  - {key}={value}");
            }
        }
        println!("turns:");
        for turn in &report.turns {
            println!(
                "[{}] {} provider={} role={} origin={} status={} started_at={} completed_at={} events={} artifacts={}",
                turn.sequence,
                turn.turn_id,
                turn.provider,
                turn.role,
                turn.origin,
                turn.status,
                turn.started_at,
                turn.completed_at.as_deref().unwrap_or("<running>"),
                turn.events.len(),
                turn.artifacts.len()
            );
            println!(
                "  duration: {}",
                turn.duration_ms
                    .map(format_duration_ms)
                    .unwrap_or_else(|| "<running>".to_string())
            );
            println!("  user: {}", preview_text(&turn.user_message, 100));
            if let Some(response) = &turn.provider_response {
                println!("  response: {}", preview_text(response, 100));
            }
            if let Some(error) = &turn.error_message {
                println!("  error: {}", preview_text(error, 100));
            }
            if let Some(delegated_by) = &turn.delegated_by {
                println!("  delegated_by: {delegated_by}");
            }
            if args.verbose {
                println!("  events ({}):", turn.events.len());
                for event in &turn.events {
                    println!(
                        "    - {} provider={} at={}",
                        event.event_type, event.provider, event.timestamp
                    );
                }
                println!("  artifacts ({}):", turn.artifacts.len());
                for artifact in &turn.artifacts {
                    println!(
                        "    - {} title={} summary={} path={}",
                        artifact.artifact_type,
                        artifact.title,
                        artifact.summary.as_deref().unwrap_or("<none>"),
                        artifact.path.as_deref().unwrap_or("<none>")
                    );
                }
            }
        }
        if !args.verbose {
            println!("turn detail: re-run with --verbose to show per-event and per-artifact rows");
        }
        Ok(())
    }
}

fn migrate_store(
    args: StoreMigrateArgs,
    config: &SwitchyardConfig,
    cwd: &Path,
) -> Result<(), String> {
    if args.dry_run && args.verify_only {
        return Err("cannot combine `--dry-run` with `--verify-only`".to_string());
    }

    let source = resolve_endpoint(
        &StoreSelectorArgs {
            backend: args.from_backend,
            path: args.from_path.clone(),
        },
        config,
        cwd,
    );
    let target = resolve_endpoint(
        &StoreSelectorArgs {
            backend: Some(args.to_backend),
            path: args.to_path.clone(),
        },
        config,
        cwd,
    );

    if source.path == target.path {
        return Err(format!(
            "source and target paths resolve to the same location: '{}'",
            source.path.display()
        ));
    }

    let source_snapshot = load_store_snapshot(&source, args.session.as_deref())?;
    let source_counts = source_snapshot.counts();
    let target_overall_snapshot = load_store_snapshot(&target, None)?;
    let selected_session = resolved_selected_session(args.session.as_deref(), &source_snapshot);
    let target_scope_snapshot = if args.session.is_some() || args.verify_only {
        load_store_snapshot_with_missing_policy(&target, args.session.as_deref(), true)?
    } else {
        target_overall_snapshot.clone()
    };
    let target_counts_before = target_scope_snapshot.counts();
    let mut warnings = Vec::new();
    let target_overall_counts = target_overall_snapshot.counts();
    let target_scope_is_empty = target_counts_before.is_empty();

    if args.verify && args.dry_run {
        warnings.push("`--verify` is ignored during `--dry-run`".to_string());
    }
    if args.verify && args.verify_only {
        warnings.push("`--verify` is redundant when `--verify-only` is used".to_string());
    }
    if source_counts.is_empty() {
        warnings.push("source store is empty; nothing would be migrated".to_string());
    }
    if !args.verify_only {
        if args.session.is_some() {
            if !target_scope_is_empty {
                warnings.push(
                    "target store already contains data for the selected session; migrate will refuse to write until that session is removed"
                        .to_string(),
                );
            } else if !target_overall_counts.is_empty() {
                warnings.push(
                    "target store already contains other sessions; selected session scope is empty so migration can proceed"
                        .to_string(),
                );
            }
        } else if !target_overall_counts.is_empty() {
            warnings.push(
                "target store is not empty; migrate will refuse to write until it is cleared"
                    .to_string(),
            );
        }
    }

    if args.verify_only {
        let verification = build_migration_verification(&source_snapshot, &target_scope_snapshot);
        let report = MigrationReport {
            mode: "verify_only".to_string(),
            selected_session: selected_session.clone(),
            from_backend: source.backend_name().to_string(),
            from_path: source.path_display(),
            from_path_exists: source_snapshot.path_exists,
            to_backend: target.backend_name().to_string(),
            to_path: target.path_display(),
            to_path_exists_before: target_overall_snapshot.path_exists,
            source_counts,
            target_counts_before,
            migrated: MigrationCounts::default(),
            can_apply: false,
            verified: true,
            verification: Some(verification.clone()),
            warnings,
        };
        emit_migration_report(&report, args.json)?;
        if verification.matches {
            return Ok(());
        }
        return Err(format!(
            "verification failed: {}",
            summarize_migration_verification(&verification)
        ));
    }

    let can_apply = !source_counts.is_empty() && target_scope_is_empty;

    if args.dry_run {
        let report = MigrationReport {
            mode: "dry_run".to_string(),
            selected_session: selected_session.clone(),
            from_backend: source.backend_name().to_string(),
            from_path: source.path_display(),
            from_path_exists: source_snapshot.path_exists,
            to_backend: target.backend_name().to_string(),
            to_path: target.path_display(),
            to_path_exists_before: target_overall_snapshot.path_exists,
            source_counts,
            target_counts_before,
            migrated: MigrationCounts::default(),
            can_apply,
            verified: false,
            verification: None,
            warnings,
        };
        return emit_migration_report(&report, args.json);
    }

    if source_counts.is_empty() {
        return Err(format!(
            "source store '{}' is empty; nothing to migrate",
            source.path.display()
        ));
    }
    if let Some(session_id) = selected_session.as_deref() {
        if !target_scope_is_empty {
            return Err(format!(
                "target store '{}' already contains data for selected session '{}'; refusing to merge into an existing session snapshot",
                target.path.display(),
                session_id
            ));
        }
    } else if !target_overall_counts.is_empty() {
        return Err(format!(
            "target store '{}' is not empty; refusing to migrate into a populated store",
            target.path.display()
        ));
    }

    let source_store = source.open()?;
    let mut target_store = target.open()?;
    let selected_session_ids = selected_session_ids(&source_store, args.session.as_deref())?;
    let total_sessions = selected_session_ids.len();

    let mut migrated = MigrationCounts::default();
    for (index, session_id) in selected_session_ids.into_iter().enumerate() {
        let Some(session) = source_store
            .load_session(session_id)
            .map_err(|err| format!("load session '{session_id}': {err}"))?
        else {
            continue;
        };

        let turns = source_store
            .list_turns(session_id)
            .map_err(|err| format!("list turns for session '{session_id}': {err}"))?;
        let events = source_store
            .list_session_events(session_id)
            .map_err(|err| format!("list events for session '{session_id}': {err}"))?;
        let mut artifact_count = 0usize;
        for turn in &turns {
            artifact_count += source_store
                .list_artifacts(turn.turn_id)
                .map_err(|err| format!("list artifacts for turn '{}': {err}", turn.turn_id))?
                .len();
        }

        if !args.json {
            println!(
                "[{}/{}] migrating session {} turns={} events={} artifacts={}",
                index + 1,
                total_sessions,
                session_id,
                turns.len(),
                events.len(),
                artifact_count,
            );
        }

        target_store
            .save_session(&session)
            .map_err(|err| format!("save session '{session_id}' to target: {err}"))?;
        migrated.sessions += 1;

        for turn in &turns {
            target_store
                .append_turn(turn)
                .map_err(|err| format!("append turn '{}' to target: {err}", turn.turn_id))?;
            migrated.turns += 1;
        }

        for event in &events {
            target_store
                .append_event(event)
                .map_err(|err| format!("append event '{}' to target: {err}", event.event_id))?;
            migrated.events += 1;
        }

        for turn in &turns {
            let artifacts = source_store
                .list_artifacts(turn.turn_id)
                .map_err(|err| format!("list artifacts for turn '{}': {err}", turn.turn_id))?;
            for artifact in &artifacts {
                target_store.save_artifact(artifact).map_err(|err| {
                    format!(
                        "append artifact '{}' to target: {err}",
                        artifact.artifact_id
                    )
                })?;
                migrated.artifacts += 1;
            }
        }
    }

    let verification = if args.verify {
        let target_after = load_store_snapshot(&target, args.session.as_deref())?;
        Some(build_migration_verification(
            &source_snapshot,
            &target_after,
        ))
    } else {
        None
    };

    if let Some(verification) = &verification
        && !verification.matches
    {
        return Err(format!(
            "migration verification failed: {}",
            summarize_migration_verification(verification),
        ));
    }

    let report = MigrationReport {
        mode: "apply".to_string(),
        selected_session,
        from_backend: source.backend_name().to_string(),
        from_path: source.path_display(),
        from_path_exists: source_snapshot.path_exists,
        to_backend: target.backend_name().to_string(),
        to_path: target.path_display(),
        to_path_exists_before: target_overall_snapshot.path_exists,
        source_counts,
        target_counts_before,
        migrated,
        can_apply: true,
        verified: verification.is_some(),
        verification,
        warnings,
    };

    emit_migration_report(&report, args.json)
}

fn emit_migration_report(report: &MigrationReport, json: bool) -> Result<(), String> {
    if json {
        print_json(report)
    } else {
        println!("mode: {}", report.mode);
        if let Some(session) = &report.selected_session {
            println!("selected session: {session}");
        }
        println!(
            "from: {} {} (exists: {})",
            report.from_backend,
            report.from_path,
            yes_no(report.from_path_exists),
        );
        println!(
            "to:   {} {} (exists before: {})",
            report.to_backend,
            report.to_path,
            yes_no(report.to_path_exists_before),
        );
        println!("source counts: {}", format_counts(&report.source_counts));
        println!(
            "target counts before: {}",
            format_counts(&report.target_counts_before)
        );
        println!("can apply: {}", yes_no(report.can_apply));
        println!(
            "migrated: sessions={} turns={} events={} artifacts={}",
            report.migrated.sessions,
            report.migrated.turns,
            report.migrated.events,
            report.migrated.artifacts,
        );
        if let Some(verification) = &report.verification {
            println!(
                "verification: {} (source: {}, target: {})",
                if verification.matches {
                    "passed"
                } else {
                    "failed"
                },
                format_counts(&verification.source),
                format_counts(&verification.target),
            );
            if !verification.missing_sessions.is_empty() {
                println!(
                    "missing target sessions: {}",
                    verification.missing_sessions.join(", ")
                );
            }
            if !verification.unexpected_sessions.is_empty() {
                println!(
                    "unexpected target sessions: {}",
                    verification.unexpected_sessions.join(", ")
                );
            }
            if !verification.mismatched_sessions.is_empty() {
                println!("mismatched sessions:");
                for mismatch in &verification.mismatched_sessions {
                    println!(
                        "  - {} source=[{}] target=[{}]",
                        mismatch.session_id,
                        format_session_signature(&mismatch.source),
                        format_session_signature(&mismatch.target),
                    );
                }
            }
        }
        if !report.warnings.is_empty() {
            println!("warnings:");
            for warning in &report.warnings {
                println!("  - {warning}");
            }
        }
        Ok(())
    }
}

fn build_migration_verification(
    source_snapshot: &StoreSnapshot,
    target_snapshot: &StoreSnapshot,
) -> MigrationVerification {
    let source = source_snapshot.counts();
    let target = target_snapshot.counts();
    let source_scope = source_snapshot.verification_scope();
    let target_scope = target_snapshot.verification_scope();

    let mut missing_sessions = Vec::new();
    let mut unexpected_sessions = Vec::new();
    let mut mismatched_sessions = Vec::new();

    for (session_id, source_entry) in &source_scope {
        match target_scope.get(session_id) {
            Some(target_entry) if target_entry == source_entry => {}
            Some(target_entry) => mismatched_sessions.push(SessionVerificationMismatch {
                session_id: session_id.clone(),
                source: source_entry.clone(),
                target: target_entry.clone(),
            }),
            None => missing_sessions.push(session_id.clone()),
        }
    }

    for session_id in target_scope.keys() {
        if !source_scope.contains_key(session_id) {
            unexpected_sessions.push(session_id.clone());
        }
    }

    let count_matches = source == target;
    let session_scope_matches = missing_sessions.is_empty()
        && unexpected_sessions.is_empty()
        && mismatched_sessions.is_empty();

    MigrationVerification {
        matches: count_matches && session_scope_matches,
        count_matches,
        session_scope_matches,
        source,
        target,
        missing_sessions,
        unexpected_sessions,
        mismatched_sessions,
    }
}

fn summarize_migration_verification(verification: &MigrationVerification) -> String {
    let mut parts = vec![format!(
        "source {} vs target {}",
        format_counts(&verification.source),
        format_counts(&verification.target)
    )];

    if !verification.missing_sessions.is_empty() {
        parts.push(format!(
            "missing sessions: {}",
            verification.missing_sessions.join(", ")
        ));
    }
    if !verification.unexpected_sessions.is_empty() {
        parts.push(format!(
            "unexpected sessions: {}",
            verification.unexpected_sessions.join(", ")
        ));
    }
    if !verification.mismatched_sessions.is_empty() {
        parts.push(format!(
            "mismatched sessions: {}",
            verification
                .mismatched_sessions
                .iter()
                .map(|item| item.session_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    parts.join("; ")
}

fn limit_session_entries(
    mut sessions: Vec<SessionListEntry>,
    limit: Option<usize>,
) -> Vec<SessionListEntry> {
    if let Some(limit) = limit {
        sessions.truncate(limit);
    }
    sessions
}

fn print_session_table(sessions: &[SessionListEntry], short: bool) {
    if short {
        println!("{:<36}  {:<16}  UPDATED_AT", "SESSION_ID", "CORE");
        for session in sessions {
            println!(
                "{:<36}  {:<16}  {}",
                session.session_id,
                truncate_display(session.active_core.as_deref().unwrap_or("<unknown>"), 16),
                session.updated_at.as_deref().unwrap_or("<unknown>")
            );
        }
    } else {
        println!(
            "{:<36}  {:<16}  {:<25}  {:>5}  {:>6}  {:>9}  {:>5}  {:>5}  {:>6}  {:>8}",
            "SESSION_ID",
            "CORE",
            "UPDATED_AT",
            "TURNS",
            "EVENTS",
            "ARTIFACTS",
            "DONE",
            "FAIL",
            "CANCEL",
            "DELEGATE",
        );
        for session in sessions {
            println!(
                "{:<36}  {:<16}  {:<25}  {:>5}  {:>6}  {:>9}  {:>5}  {:>5}  {:>6}  {:>8}",
                session.session_id,
                truncate_display(session.active_core.as_deref().unwrap_or("<unknown>"), 16),
                session.updated_at.as_deref().unwrap_or("<unknown>"),
                session.turns,
                session.events,
                session.artifacts,
                session.completed_turns,
                session.failed_turns,
                session.cancelled_turns,
                session.delegate_turns,
            );
        }
    }
}

fn truncate_display(value: &str, max_chars: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= max_chars {
        return value.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    if max_chars == 1 {
        return "…".to_string();
    }

    let mut truncated = value.chars().take(max_chars - 1).collect::<String>();
    truncated.push('…');
    truncated
}

fn format_session_signature(session: &SessionListEntry) -> String {
    format!(
        "core={} updated_at={} turns={} events={} artifacts={} completed={} failed={} cancelled={} delegate_turns={}",
        session.active_core.as_deref().unwrap_or("<unknown>"),
        session.updated_at.as_deref().unwrap_or("<unknown>"),
        session.turns,
        session.events,
        session.artifacts,
        session.completed_turns,
        session.failed_turns,
        session.cancelled_turns,
        session.delegate_turns,
    )
}

fn load_store_snapshot(
    endpoint: &ResolvedStoreEndpoint,
    session_filter: Option<&str>,
) -> Result<StoreSnapshot, String> {
    load_store_snapshot_with_missing_policy(endpoint, session_filter, false)
}

fn load_store_snapshot_with_missing_policy(
    endpoint: &ResolvedStoreEndpoint,
    session_filter: Option<&str>,
    missing_session_is_empty: bool,
) -> Result<StoreSnapshot, String> {
    let path_exists = endpoint.path.exists();
    if endpoint.backend == StoreBackend::Sqlite && !path_exists {
        return Ok(StoreSnapshot {
            path_exists,
            sessions: Vec::new(),
            sqlite_schema: None,
        });
    }

    let store = endpoint.open()?;
    let sessions = collect_session_entries_with_missing_policy(
        &store,
        session_filter,
        missing_session_is_empty,
    )?;
    let sqlite_schema = store
        .sqlite_schema_info()
        .map_err(|err| format!("inspect sqlite schema '{}': {err}", endpoint.path.display()))?;

    Ok(StoreSnapshot {
        path_exists,
        sessions,
        sqlite_schema,
    })
}

fn collect_session_entries_with_missing_policy(
    store: &StoreHandle,
    session_filter: Option<&str>,
    missing_session_is_empty: bool,
) -> Result<Vec<SessionListEntry>, String> {
    let mut entries = Vec::new();
    for session_id in
        selected_session_ids_with_missing_policy(store, session_filter, missing_session_is_empty)?
    {
        let session = store
            .load_session(session_id)
            .map_err(|err| format!("load session '{session_id}': {err}"))?;
        let turns = store
            .list_turns(session_id)
            .map_err(|err| format!("list turns for session '{session_id}': {err}"))?;
        let events = store
            .list_session_events(session_id)
            .map_err(|err| format!("list events for session '{session_id}': {err}"))?;

        let artifacts = turns.iter().try_fold(0usize, |acc, turn| {
            store
                .list_artifacts(turn.turn_id)
                .map(|items| acc + items.len())
                .map_err(|err| format!("list artifacts for turn '{}': {err}", turn.turn_id))
        })?;

        entries.push(SessionListEntry {
            session_id: session_id.to_string(),
            active_core: session.as_ref().map(|value| value.active_core.clone()),
            updated_at: session.as_ref().map(|value| value.updated_at.to_rfc3339()),
            turns: turns.len(),
            events: events.len(),
            artifacts,
            completed_turns: turns
                .iter()
                .filter(|turn| matches!(turn.status, switchyard_session::TurnStatus::Completed))
                .count(),
            failed_turns: turns
                .iter()
                .filter(|turn| matches!(turn.status, switchyard_session::TurnStatus::Failed))
                .count(),
            cancelled_turns: turns
                .iter()
                .filter(|turn| matches!(turn.status, switchyard_session::TurnStatus::Cancelled))
                .count(),
            delegate_turns: turns
                .iter()
                .filter(|turn| matches!(turn.origin, switchyard_session::TurnOrigin::Delegate))
                .count(),
        });
    }

    entries.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    Ok(entries)
}

fn selected_session_ids(
    store: &StoreHandle,
    session_filter: Option<&str>,
) -> Result<Vec<Uuid>, String> {
    selected_session_ids_with_missing_policy(store, session_filter, false)
}

fn selected_session_ids_with_missing_policy(
    store: &StoreHandle,
    session_filter: Option<&str>,
    missing_session_is_empty: bool,
) -> Result<Vec<Uuid>, String> {
    match session_filter {
        Some(session_filter) => {
            resolve_session_selector(store, session_filter, missing_session_is_empty)
        }
        None => store
            .list_sessions()
            .map_err(|err| format!("list sessions: {err}")),
    }
}

fn resolve_session_selector(
    store: &StoreHandle,
    session_filter: &str,
    missing_session_is_empty: bool,
) -> Result<Vec<Uuid>, String> {
    let normalized = session_filter.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err("`--session` cannot be empty".to_string());
    }

    let session_ids = store
        .list_sessions()
        .map_err(|err| format!("list sessions: {err}"))?;
    let matches = session_ids
        .into_iter()
        .filter(|session_id| session_id.to_string().starts_with(&normalized))
        .collect::<Vec<_>>();

    match matches.len() {
        0 if missing_session_is_empty => Ok(Vec::new()),
        0 => Err(format!(
            "session '{session_filter}' not found in selected store"
        )),
        1 => Ok(matches),
        _ => Err(format!(
            "session prefix '{session_filter}' is ambiguous; matches: {}",
            matches
                .iter()
                .map(Uuid::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn resolved_selected_session(
    session_filter: Option<&str>,
    snapshot: &StoreSnapshot,
) -> Option<String> {
    session_filter.and_then(|_| {
        snapshot
            .sessions
            .first()
            .map(|session| session.session_id.clone())
    })
}

impl StoreShowArgs {
    fn requested_session(&self) -> Option<&str> {
        self.session.as_deref().or(self.session_ref.as_deref())
    }
}

fn resolve_store_show_selection(
    store: &StoreHandle,
    args: &StoreShowArgs,
) -> Result<(Uuid, SessionSelectionReport), String> {
    if let Some(session_filter) = args.requested_session() {
        let normalized = session_filter.trim();
        if normalized.is_empty() {
            return Err("`--session` cannot be empty".to_string());
        }
        let selected = selected_session_ids(store, Some(normalized))?;
        let session_id = selected
            .into_iter()
            .next()
            .ok_or_else(|| format!("session '{normalized}' not found in selected store"))?;
        return Ok((
            session_id,
            SessionSelectionReport {
                mode: "explicit".to_string(),
                query: Some(normalized.to_string()),
            },
        ));
    }

    let latest = collect_session_entries_with_missing_policy(store, None, false)?;
    let session = latest
        .into_iter()
        .next()
        .ok_or_else(|| "selected store contains no sessions".to_string())?;
    let session_id = session
        .session_id
        .parse::<Uuid>()
        .map_err(|err| format!("parse latest session id '{}': {err}", session.session_id))?;
    Ok((
        session_id,
        SessionSelectionReport {
            mode: "latest".to_string(),
            query: None,
        },
    ))
}

fn format_store_show_selection(selection: &SessionSelectionReport) -> String {
    match selection.mode.as_str() {
        "explicit" => format!(
            "requested session {}",
            selection.query.as_deref().unwrap_or("<unknown>")
        ),
        "latest" => "latest updated session".to_string(),
        other => other.to_string(),
    }
}

fn resolve_endpoint(
    selector: &StoreSelectorArgs,
    config: &SwitchyardConfig,
    cwd: &Path,
) -> ResolvedStoreEndpoint {
    let configured_backend = config.store_backend(cwd);
    let backend = selector
        .backend
        .map(StoreBackend::from)
        .unwrap_or(configured_backend);
    let path = match selector.path.as_ref() {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => cwd.join(path),
        None if backend == configured_backend => config.store_path(cwd),
        None => default_path_for_backend(config, cwd, backend),
    };

    ResolvedStoreEndpoint { backend, path }
}

fn default_path_for_backend(
    config: &SwitchyardConfig,
    cwd: &Path,
    backend: StoreBackend,
) -> PathBuf {
    match backend {
        StoreBackend::Jsonl => config.session_dir(cwd),
        StoreBackend::Sqlite => cwd.join(DOT_SWITCHYARD_DIR).join("store.sqlite3"),
    }
}

fn format_counts(counts: &StoreCounts) -> String {
    format!(
        "sessions={} turns={} events={} artifacts={}",
        counts.sessions, counts.turns, counts.events, counts.artifacts
    )
}

fn join_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "<none>".to_string()
    } else {
        values.join(", ")
    }
}

fn preview_text(value: &str, max_chars: usize) -> String {
    truncate_display(&collapse_whitespace(value), max_chars)
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_duration_ms(duration_ms: i64) -> String {
    if duration_ms < 1_000 {
        format!("{duration_ms}ms")
    } else {
        format!("{:.2}s", duration_ms as f64 / 1_000.0)
    }
}

fn backend_name(backend: StoreBackend) -> &'static str {
    match backend {
        StoreBackend::Jsonl => "jsonl",
        StoreBackend::Sqlite => "sqlite",
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn print_json<T: Serialize>(value: &T) -> Result<(), String> {
    let payload =
        serde_json::to_string_pretty(value).map_err(|err| format!("serialize output: {err}"))?;
    println!("{payload}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_endpoint_uses_configured_store_path_when_backend_matches() {
        let mut config = SwitchyardConfig::default();
        config.store.path = Some(PathBuf::from("custom/store.sqlite3"));
        config.store.backend = switchyard_config::StoreBackendConfig::Sqlite;

        let resolved = resolve_endpoint(
            &StoreSelectorArgs {
                backend: None,
                path: None,
            },
            &config,
            Path::new("/project"),
        );

        assert_eq!(resolved.backend, StoreBackend::Sqlite);
        assert_eq!(
            resolved.path,
            PathBuf::from("/project/custom/store.sqlite3")
        );
    }

    #[test]
    fn resolve_endpoint_switches_to_backend_specific_default_path_when_overridden() {
        let mut config = SwitchyardConfig::default();
        config.store.backend = switchyard_config::StoreBackendConfig::Sqlite;
        config.store.path = Some(PathBuf::from("configured/store.sqlite3"));

        let resolved = resolve_endpoint(
            &StoreSelectorArgs {
                backend: Some(CliStoreBackend::Jsonl),
                path: None,
            },
            &config,
            Path::new("/project"),
        );

        assert_eq!(resolved.backend, StoreBackend::Jsonl);
        assert_eq!(
            resolved.path,
            PathBuf::from("/project/.switchyard/sessions")
        );
    }

    #[test]
    fn store_counts_is_empty_requires_all_zero() {
        assert!(StoreCounts::default().is_empty());
        assert!(
            !StoreCounts {
                sessions: 1,
                ..StoreCounts::default()
            }
            .is_empty()
        );
    }
}
