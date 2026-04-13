use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use switchyard_config::SwitchyardConfig;
use switchyard_store::{
    ArtifactStore, EventLog, SessionCatalog, SessionEventRepository, SessionRepository,
    StoreBackend, StoreHandle, TurnRepository,
};

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

    /// Destination backend.
    #[arg(long = "to-backend", value_enum)]
    to_backend: CliStoreBackend,

    /// Destination path override. Relative paths resolve from the current project root.
    #[arg(long = "to-path")]
    to_path: Option<PathBuf>,

    /// Preview counts and validation outcome without writing target data.
    #[arg(long)]
    dry_run: bool,

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

#[derive(Debug, Clone, Serialize)]
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
    sessions: Vec<SessionListEntry>,
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
    source: StoreCounts,
    target: StoreCounts,
}

#[derive(Debug, Clone, Serialize)]
struct MigrationReport {
    mode: String,
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
        StoreAction::Migrate(args) => migrate_store(args, config, cwd),
    }
}

fn inspect_store(
    args: StoreInspectArgs,
    config: &SwitchyardConfig,
    cwd: &Path,
) -> Result<(), String> {
    let endpoint = resolve_endpoint(&args.selector, config, cwd);
    let snapshot = load_store_snapshot(&endpoint)?;
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
    let endpoint = resolve_endpoint(&args.selector, config, cwd);
    let snapshot = load_store_snapshot(&endpoint)?;
    let report = SessionListReport {
        backend: endpoint.backend_name().to_string(),
        path: endpoint.path_display(),
        path_exists: snapshot.path_exists,
        sessions: snapshot.sessions,
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
        for session in &report.sessions {
            println!(
                "{} core={} updated_at={} turns={} events={} artifacts={} completed={} failed={} cancelled={} delegate_turns={}",
                session.session_id,
                session.active_core.as_deref().unwrap_or("<unknown>"),
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
        Ok(())
    }
}

fn migrate_store(
    args: StoreMigrateArgs,
    config: &SwitchyardConfig,
    cwd: &Path,
) -> Result<(), String> {
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

    let source_snapshot = load_store_snapshot(&source)?;
    let target_snapshot = load_store_snapshot(&target)?;
    let source_counts = source_snapshot.counts();
    let target_counts_before = target_snapshot.counts();
    let mut warnings = Vec::new();

    if args.verify && args.dry_run {
        warnings.push("`--verify` is ignored during `--dry-run`".to_string());
    }
    if source_counts.is_empty() {
        warnings.push("source store is empty; nothing would be migrated".to_string());
    }
    if !target_counts_before.is_empty() {
        warnings.push(
            "target store is not empty; migrate will refuse to write until it is cleared"
                .to_string(),
        );
    }

    let can_apply = !source_counts.is_empty() && target_counts_before.is_empty();

    if args.dry_run {
        let report = MigrationReport {
            mode: "dry_run".to_string(),
            from_backend: source.backend_name().to_string(),
            from_path: source.path_display(),
            from_path_exists: source_snapshot.path_exists,
            to_backend: target.backend_name().to_string(),
            to_path: target.path_display(),
            to_path_exists_before: target_snapshot.path_exists,
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
    if !target_counts_before.is_empty() {
        return Err(format!(
            "target store '{}' is not empty; refusing to migrate into a populated store",
            target.path.display()
        ));
    }

    let source_store = source.open()?;
    let mut target_store = target.open()?;
    let session_ids = source_store
        .list_sessions()
        .map_err(|err| format!("list source sessions '{}': {err}", source.path.display()))?;

    let mut migrated = MigrationCounts::default();
    for session_id in session_ids {
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
        let target_after = load_store_snapshot(&target)?;
        let target_counts = target_after.counts();
        Some(MigrationVerification {
            matches: target_counts == source_counts,
            source: source_counts.clone(),
            target: target_counts,
        })
    } else {
        None
    };

    if let Some(verification) = &verification
        && !verification.matches
    {
        return Err(format!(
            "migration verification failed: source counts {} != target counts {}",
            format_counts(&verification.source),
            format_counts(&verification.target),
        ));
    }

    let report = MigrationReport {
        mode: "apply".to_string(),
        from_backend: source.backend_name().to_string(),
        from_path: source.path_display(),
        from_path_exists: source_snapshot.path_exists,
        to_backend: target.backend_name().to_string(),
        to_path: target.path_display(),
        to_path_exists_before: target_snapshot.path_exists,
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
                "verification: {}",
                if verification.matches {
                    "passed"
                } else {
                    "failed"
                }
            );
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

fn load_store_snapshot(endpoint: &ResolvedStoreEndpoint) -> Result<StoreSnapshot, String> {
    let path_exists = endpoint.path.exists();
    if endpoint.backend == StoreBackend::Sqlite && !path_exists {
        return Ok(StoreSnapshot {
            path_exists,
            sessions: Vec::new(),
            sqlite_schema: None,
        });
    }

    let store = endpoint.open()?;
    let sessions = collect_session_entries(&store)?;
    let sqlite_schema = store
        .sqlite_schema_info()
        .map_err(|err| format!("inspect sqlite schema '{}': {err}", endpoint.path.display()))?;

    Ok(StoreSnapshot {
        path_exists,
        sessions,
        sqlite_schema,
    })
}

fn collect_session_entries(store: &StoreHandle) -> Result<Vec<SessionListEntry>, String> {
    let mut entries = Vec::new();
    for session_id in store
        .list_sessions()
        .map_err(|err| format!("list sessions: {err}"))?
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

fn resolve_endpoint(
    selector: &StoreSelectorArgs,
    config: &SwitchyardConfig,
    cwd: &Path,
) -> ResolvedStoreEndpoint {
    let configured_backend = config.store_backend();
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
