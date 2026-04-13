//! `switchyard host` subcommand — machine-readable bridge for host packs.
//!
//! All machine-readable output is emitted as a single JSON document on stdout.
//! Exit 0 = success, non-zero = error.
//! Host packs (Claude/Gemini/Codex) call these subcommands to implement /hyard:*.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use std::{io::Write, process};

use chrono::Utc;
use switchyard_config::SwitchyardConfig;
use switchyard_core::{
    ProviderRegistry, RuntimeEvent, TurnPhase, build_peer_catalog_probed, run_turn_phased,
};
#[cfg(test)]
use switchyard_host_jobs::refresh_orphaned_job_state_with;
use switchyard_host_jobs::{
    HostJobState, HostJobStatus, HostJobStore, HostJobWorkerMode, load_job_with_refresh,
};
use switchyard_provider_api::{CancellationToken, PromptMode, Provider};
use switchyard_session::{Session, TurnStatus};
use switchyard_store::{
    ArtifactStore, SessionCatalog, SessionRepository, StoreHandle, TurnRepository,
};
use tokio::sync::mpsc;
use uuid::Uuid;

pub const DEFAULT_WAIT_SECS: u64 = 30;
const HYARD_PROTOCOL: &str = "hyard_v2";

const POLL_INTERVAL_MS: u64 = 200;
const INLINE_WORKER_ENV: &str = "SWITCHYARD_HOST_WORKER_INLINE";
const WORKER_BOOT_TIMEOUT_MS: u64 = 1_500;
const WORKER_BOOT_POLL_MS: u64 = 50;
const WORKER_LOG_TAIL_BYTES: usize = 8 * 1024;

fn open_configured_store(config: &SwitchyardConfig, cwd: &Path) -> Result<StoreHandle, String> {
    StoreHandle::open(config.store_backend(), config.store_path(cwd))
        .map_err(|err| format!("open store: {err}"))
}

/// Execute `switchyard host list` — list available peers with probe status.
pub async fn host_list(registry: &ProviderRegistry, config: &SwitchyardConfig) {
    let catalog = build_peer_catalog_probed(
        "", // no active core exclusion for host list
        registry,
        &config.providers,
    )
    .await;

    let peers: Vec<serde_json::Value> = catalog
        .peers
        .iter()
        .map(|p| {
            serde_json::json!({
                "provider": p.provider_id,
                "available": p.available,
                "roles": p.roles.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
            })
        })
        .collect();

    emit_json(&serde_json::json!({
        "protocol": HYARD_PROTOCOL,
        "command": "list",
        "peers": peers,
    }));
}

/// Execute `switchyard host delegate` with the default wait window.
#[allow(dead_code)]
pub async fn host_delegate(
    registry: &ProviderRegistry,
    config: &SwitchyardConfig,
    provider_name: &str,
    task: &str,
    cwd: &Path,
) {
    host_delegate_with_wait(
        registry,
        config,
        provider_name,
        task,
        cwd,
        DEFAULT_WAIT_SECS,
    )
    .await;
}

/// Execute `switchyard host delegate` — submit a leaf peer job and wait briefly.
///
/// If the peer finishes within `wait_secs`, the completed result is returned.
/// Otherwise the command returns `wait_timeout` while the job continues running
/// in the background and can later be inspected via status/result/await/cancel.
pub async fn host_delegate_with_wait(
    registry: &ProviderRegistry,
    config: &SwitchyardConfig,
    provider_name: &str,
    task: &str,
    cwd: &Path,
    wait_secs: u64,
) {
    let job_store = HostJobStore::new(config.job_dir(cwd));

    if registry
        .create(provider_name, config.providers.get(provider_name))
        .is_none()
    {
        print_error(
            "provider_unavailable",
            &format!("provider '{provider_name}' not registered"),
        );
        std::process::exit(1);
    }

    let mut job = HostJobState::new(
        provider_name.to_string(),
        task.to_string(),
        cwd.to_path_buf(),
    );
    job.worker_mode = Some(if should_run_worker_inline() {
        HostJobWorkerMode::Inline
    } else {
        HostJobWorkerMode::Subprocess
    });
    if let Err(err) = job_store.save(&job) {
        print_error("execution_failed", &format!("job init: {err}"));
        std::process::exit(1);
    }

    let launch_result = if should_run_worker_inline() {
        let prepared = match prepare_host_job_run(registry, config, provider_name, task, cwd).await
        {
            Ok(prepared) => prepared,
            Err(err) => {
                mark_job_failed(&job_store, job.job_id, &err);
                print_error("execution_failed", &err);
                std::process::exit(1);
            }
        };

        let job_store_for_task = job_store.clone();
        let job_id = job.job_id;
        tokio::spawn(async move {
            if let Err(err) = run_host_job(job_store_for_task.clone(), job_id, prepared).await {
                mark_job_failed(&job_store_for_task, job_id, &err);
            }
        });

        Ok(())
    } else {
        spawn_worker_subprocess(&job_store, job.job_id, cwd)
    };

    if let Err(err) = launch_result {
        mark_job_failed(&job_store, job.job_id, &err);
        print_error("execution_failed", &err);
        std::process::exit(1);
    }

    emit_wait_result(&job_store, job.job_id, wait_secs, "delegate").await;
}

/// Execute `switchyard host worker` — background job executor.
pub async fn host_worker(
    registry: &ProviderRegistry,
    config: &SwitchyardConfig,
    job_id: &str,
    cwd: &Path,
) {
    let parsed_job_id = match parse_job_id(job_id) {
        Ok(job_id) => job_id,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };

    let job_store = HostJobStore::new(config.job_dir(cwd));
    let job = match job_store.load(parsed_job_id) {
        Ok(Some(job)) => job,
        Ok(None) => {
            eprintln!("job '{parsed_job_id}' not found");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("failed to load job '{parsed_job_id}': {err}");
            std::process::exit(1);
        }
    };

    if job.status.is_terminal() {
        return;
    }

    if job.status == HostJobStatus::CancelRequested {
        let _ = job_store.update(parsed_job_id, |job| {
            job.status = HostJobStatus::Cancelled;
            job.completed_at = Some(Utc::now());
            job.last_event = Some("cancelled_before_start".to_string());
            job.error = Some("cancelled".to_string());
        });
        return;
    }

    let worker_pid = std::process::id();
    let _ = job_store.update(parsed_job_id, |job| {
        job.pid = Some(worker_pid);
        if matches!(job.status, HostJobStatus::Queued) {
            job.status = HostJobStatus::Running;
        }
        if job.started_at.is_none() {
            job.started_at = Some(Utc::now());
        }
        if !job.status.is_terminal() {
            job.last_event = Some("worker_booting".to_string());
        }
    });

    let prepared =
        match prepare_host_job_run(registry, config, &job.provider, &job.task, &job.cwd).await {
            Ok(prepared) => prepared,
            Err(err) => {
                mark_job_failed(&job_store, parsed_job_id, &err);
                return;
            }
        };

    if let Err(err) = run_host_job(job_store.clone(), parsed_job_id, prepared).await {
        mark_job_failed(&job_store, parsed_job_id, &err);
    }
}

/// Execute `switchyard host status` — check a job's status.
pub async fn host_status(config: &SwitchyardConfig, job_id: &str, cwd: &Path) {
    let store = HostJobStore::new(config.job_dir(cwd));
    if let Some(job) = load_job_or_exit(&store, job_id) {
        emit_json(&job_bridge_json("status", &job));
        return;
    }

    let store = open_configured_store(config, cwd).unwrap_or_else(|err| {
        print_error("execution_failed", &err);
        process::exit(1);
    });
    let turn = find_turn_by_id(&store, job_id);
    match turn {
        Some(t) => emit_json(&legacy_turn_bridge_json("status", job_id, &t)),
        None => {
            print_error("not_found", &format!("job '{job_id}' not found"));
            process::exit(1);
        }
    }
}

/// Execute `switchyard host await` — wait again on an existing async job.
pub async fn host_await(config: &SwitchyardConfig, job_id: &str, cwd: &Path, timeout_secs: u64) {
    let parsed_job_id = match parse_job_id(job_id) {
        Ok(job_id) => job_id,
        Err(message) => {
            print_error("invalid_job_id", &message);
            process::exit(1);
        }
    };

    let job_store = HostJobStore::new(config.job_dir(cwd));
    match load_job_with_refresh(&job_store, parsed_job_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            print_error("not_found", &format!("job '{job_id}' not found"));
            process::exit(1);
        }
        Err(err) => {
            print_error("execution_failed", &format!("load job '{job_id}': {err}"));
            process::exit(1);
        }
    }

    emit_wait_result(&job_store, parsed_job_id, timeout_secs, "await").await;
}

/// Execute `switchyard host result` — get full result of a job.
pub async fn host_result(config: &SwitchyardConfig, job_id: &str, cwd: &Path) {
    let store = HostJobStore::new(config.job_dir(cwd));
    if let Some(job) = load_job_or_exit(&store, job_id) {
        emit_json(&job_bridge_json("result", &job));
        return;
    }

    let store = open_configured_store(config, cwd).unwrap_or_else(|err| {
        print_error("execution_failed", &err);
        process::exit(1);
    });
    let turn = find_turn_by_id(&store, job_id);
    match turn {
        Some(t) => emit_json(&legacy_turn_bridge_json("result", job_id, &t)),
        None => {
            print_error("not_found", &format!("job '{job_id}' not found"));
            process::exit(1);
        }
    }
}

/// Execute `switchyard host cancel` — request cancellation of a running job.
pub async fn host_cancel(config: &SwitchyardConfig, job_id: &str, cwd: &Path) {
    let parsed_job_id = match parse_job_id(job_id) {
        Ok(job_id) => job_id,
        Err(_) => {
            let store = open_configured_store(config, cwd).unwrap_or_else(|err| {
                print_error("execution_failed", &err);
                process::exit(1);
            });
            let turn = find_turn_by_id(&store, job_id);
            match turn {
                Some(t) => emit_json(&legacy_turn_bridge_json("cancel", job_id, &t)),
                None => {
                    print_error("not_found", &format!("job '{job_id}' not found"));
                    process::exit(1);
                }
            }
            return;
        }
    };

    let store = HostJobStore::new(config.job_dir(cwd));
    match load_job_with_refresh(&store, parsed_job_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            let store = open_configured_store(config, cwd).unwrap_or_else(|err| {
                print_error("execution_failed", &err);
                process::exit(1);
            });
            let turn = find_turn_by_id(&store, job_id);
            match turn {
                Some(t) => emit_json(&legacy_turn_bridge_json("cancel", job_id, &t)),
                None => {
                    print_error("not_found", &format!("job '{job_id}' not found"));
                    process::exit(1);
                }
            }
            return;
        }
        Err(err) => {
            print_error("execution_failed", &format!("load job '{job_id}': {err}"));
            process::exit(1);
        }
    }

    let updated = match store.update(parsed_job_id, |job| {
        if !job.status.is_terminal() {
            job.status = HostJobStatus::CancelRequested;
            job.last_event = Some("cancel_requested".to_string());
        }
    }) {
        Ok(job) => job,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            print_error("not_found", &format!("job '{job_id}' not found"));
            process::exit(1);
        }
        Err(err) => {
            print_error("execution_failed", &format!("cancel job '{job_id}': {err}"));
            process::exit(1);
        }
    };

    emit_json(&job_bridge_json("cancel", &updated));
}

/// Look up a turn by its string ID across all sessions.
fn find_turn_by_id(
    store: &(impl SessionCatalog + TurnRepository + ?Sized),
    job_id: &str,
) -> Option<switchyard_session::Turn> {
    for sid in store.list_sessions().unwrap_or_default() {
        if let Ok(turns) = store.list_turns(sid)
            && let Some(turn) = turns.into_iter().find(|t| t.turn_id.to_string() == job_id)
        {
            return Some(turn);
        }
    }
    None
}

/// Execute `switchyard host help` — print structured command reference.
pub fn host_help() {
    let commands = serde_json::json!({
        "protocol": "hyard_v2",
        "commands": [
            {
                "name": "/hyard:list",
                "cli": "switchyard host list",
                "description": "List available peer providers with probe status",
            },
            {
                "name": "/hyard:delegate",
                "cli": "switchyard host delegate --provider <name> --task <text> [--wait-sec <n>]",
                "description": "Submit a peer leaf job and wait briefly; returns wait_timeout if still running",
            },
            {
                "name": "/hyard:status",
                "cli": "switchyard host status --job-id <uuid>",
                "description": "Check the current status and last observed activity of a delegate job",
            },
            {
                "name": "/hyard:await",
                "cli": "switchyard host await --job-id <uuid> [--timeout-sec <n>]",
                "description": "Wait longer on an already-running job without restarting it",
            },
            {
                "name": "/hyard:result",
                "cli": "switchyard host result --job-id <uuid>",
                "description": "Get the latest known result payload for a job",
            },
            {
                "name": "/hyard:cancel",
                "cli": "switchyard host cancel --job-id <uuid>",
                "description": "Request cancellation of a running delegate job",
            },
            {
                "name": "/hyard:help",
                "cli": "switchyard host help",
                "description": "Print this command reference",
            },
        ],
    });
    println!("{}", serde_json::to_string_pretty(&commands).unwrap());
}

fn print_error(code: &str, message: &str) {
    emit_json(&serde_json::json!({
        "protocol": HYARD_PROTOCOL,
        "command": "error",
        "error": code,
        "message": message,
    }));
}

fn emit_json(value: &serde_json::Value) {
    let rendered = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(rendered.as_bytes());
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();
}

fn job_bridge_json(command: &'static str, job: &HostJobState) -> serde_json::Value {
    augment_bridge_response(
        command,
        job.to_bridge_json(),
        Some(job.status),
        job.result_ready,
    )
}

fn wait_timeout_bridge_json(command: &'static str, job: &HostJobState) -> serde_json::Value {
    augment_bridge_response(
        command,
        job.to_wait_timeout_json(),
        Some(job.status),
        job.result_ready,
    )
}

fn legacy_turn_bridge_json(
    command: &'static str,
    job_id: &str,
    turn: &switchyard_session::Turn,
) -> serde_json::Value {
    let result_ready = matches!(turn.status, TurnStatus::Completed);
    augment_bridge_response(command, legacy_turn_json(job_id, turn), None, result_ready)
}

fn augment_bridge_response(
    command: &'static str,
    mut value: serde_json::Value,
    live_status: Option<HostJobStatus>,
    result_ready: bool,
) -> serde_json::Value {
    let Some(obj) = value.as_object_mut() else {
        return value;
    };

    let status = obj
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let provider = obj
        .get("provider")
        .and_then(|value| value.as_str())
        .unwrap_or("peer")
        .to_string();

    obj.insert("protocol".to_string(), serde_json::json!(HYARD_PROTOCOL));
    obj.insert("command".to_string(), serde_json::json!(command));
    obj.insert(
        "message".to_string(),
        serde_json::json!(bridge_message(
            command,
            &provider,
            &status,
            live_status,
            result_ready
        )),
    );
    obj.insert(
        "next_actions".to_string(),
        serde_json::json!(bridge_next_actions(&status, live_status, result_ready)),
    );
    value
}

fn bridge_message(
    command: &str,
    provider: &str,
    status: &str,
    live_status: Option<HostJobStatus>,
    result_ready: bool,
) -> String {
    let provider = display_provider_name(provider);

    match status {
        "wait_timeout" => format!(
            "{provider} job is still running in background; reuse the same job_id with status/result/await."
        ),
        "completed" if result_ready => match command {
            "result" => format!("{provider} result is ready and returned in this payload."),
            "cancel" => format!("{provider} job had already completed before cancel."),
            _ => format!("{provider} job completed successfully."),
        },
        "failed" => format!("{provider} job failed; inspect error and decide whether to retry."),
        "cancelled" => format!("{provider} job is cancelled."),
        "cancel_requested" => {
            format!("{provider} cancel request recorded; poll status/result with the same job_id.")
        }
        "running" | "queued" => {
            if command == "result" && !result_ready {
                format!(
                    "{provider} result is not ready yet; the same job_id is still active in background."
                )
            } else {
                format!(
                    "{provider} job is still active; continue with status/result/await using the same job_id."
                )
            }
        }
        other => {
            if let Some(HostJobStatus::Running | HostJobStatus::Queued) = live_status {
                format!(
                    "{provider} job is still active ({other}); continue with status/result/await using the same job_id."
                )
            } else {
                format!("{provider} bridge state: {other}.")
            }
        }
    }
}

fn bridge_next_actions(
    status: &str,
    live_status: Option<HostJobStatus>,
    result_ready: bool,
) -> Vec<&'static str> {
    match status {
        "wait_timeout" => vec!["status", "result", "await", "cancel"],
        "queued" | "running" => vec!["status", "result", "await", "cancel"],
        "cancel_requested" => vec!["status", "result"],
        "completed" if result_ready => vec!["result"],
        "failed" | "cancelled" => vec![],
        _ => match live_status {
            Some(HostJobStatus::Queued | HostJobStatus::Running) => {
                vec!["status", "result", "await", "cancel"]
            }
            Some(HostJobStatus::CancelRequested) => vec!["status", "result"],
            Some(HostJobStatus::Completed) if result_ready => vec!["result"],
            _ => vec![],
        },
    }
}

fn display_provider_name(provider: &str) -> String {
    match provider.to_ascii_lowercase().as_str() {
        "claude" => "Claude".to_string(),
        "codex" => "Codex".to_string(),
        "gemini" => "Gemini".to_string(),
        other => other.to_string(),
    }
}

fn parse_job_id(job_id: &str) -> Result<Uuid, String> {
    job_id
        .parse::<Uuid>()
        .map_err(|_| format!("invalid job id '{job_id}'"))
}

fn should_run_worker_inline() -> bool {
    if std::env::var_os(INLINE_WORKER_ENV).is_some() {
        return true;
    }

    let exe_name = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_stem().map(|value| value.to_owned()))
        .and_then(|value| value.to_str().map(|value| value.to_ascii_lowercase()));

    !matches!(exe_name.as_deref(), Some("switchyard"))
}

fn load_job_or_exit(job_store: &HostJobStore, job_id: &str) -> Option<HostJobState> {
    let parsed_job_id = match parse_job_id(job_id) {
        Ok(job_id) => job_id,
        Err(_) => return None,
    };

    match load_job_with_refresh(job_store, parsed_job_id) {
        Ok(job) => job,
        Err(err) => {
            print_error("execution_failed", &format!("load job '{job_id}': {err}"));
            process::exit(1);
        }
    }
}

fn spawn_worker_subprocess(
    job_store: &HostJobStore,
    job_id: Uuid,
    cwd: &Path,
) -> Result<(), String> {
    job_store
        .ensure_dir()
        .map_err(|err| format!("ensure job directory: {err}"))?;
    job_store
        .update(job_id, |job| {
            if !job.status.is_terminal() {
                job.last_event = Some("worker_launching".to_string());
            }
        })
        .map_err(|err| format!("mark worker launching: {err}"))?;

    let exe =
        std::env::current_exe().map_err(|err| format!("resolve current executable: {err}"))?;
    let stdout_log_path = job_store.worker_stdout_path(job_id);
    let stderr_log_path = job_store.worker_stderr_path(job_id);
    let stdout_log = std::fs::File::create(&stdout_log_path).map_err(|err| {
        format!(
            "create worker stdout log '{}': {err}",
            stdout_log_path.display()
        )
    })?;
    let stderr_log = std::fs::File::create(&stderr_log_path).map_err(|err| {
        format!(
            "create worker stderr log '{}': {err}",
            stderr_log_path.display()
        )
    })?;

    let mut command = std::process::Command::new(exe);
    command
        .args(["host", "worker", "--job-id", &job_id.to_string()])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log));

    let mut child = command
        .spawn()
        .map_err(|err| format!("spawn host worker: {err}"))?;
    let child_pid = child.id();

    job_store
        .update(job_id, |job| {
            if !job.status.is_terminal() {
                job.pid = Some(child_pid);
                job.last_event = Some("worker_spawned".to_string());
            }
        })
        .map_err(|err| format!("record spawned worker pid: {err}"))?;

    wait_for_worker_boot(job_store, job_id, &mut child)?;
    Ok(())
}

fn wait_for_worker_boot(
    job_store: &HostJobStore,
    job_id: Uuid,
    child: &mut std::process::Child,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_millis(WORKER_BOOT_TIMEOUT_MS);

    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| format!("check host worker status: {err}"))?
        {
            let job = job_store
                .load(job_id)
                .map_err(|err| format!("load worker manifest '{job_id}': {err}"))?
                .ok_or_else(|| format!("job '{job_id}' disappeared during worker startup"))?;

            if is_stalled_at_worker_boot(&job) {
                return Err(build_worker_boot_failure_message(job_store, job_id, status));
            }

            return Ok(());
        }

        let job = job_store
            .load(job_id)
            .map_err(|err| format!("load worker manifest '{job_id}': {err}"))?
            .ok_or_else(|| format!("job '{job_id}' disappeared during worker startup"))?;

        if job.status != HostJobStatus::Queued
            || job.session_id.is_some()
            || matches!(
                job.last_event.as_deref(),
                Some("worker_started") | Some("worker_booting")
            )
        {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Ok(());
        }

        std::thread::sleep(Duration::from_millis(WORKER_BOOT_POLL_MS));
    }
}

fn is_stalled_at_worker_boot(job: &HostJobState) -> bool {
    matches!(job.status, HostJobStatus::Queued)
        && matches!(
            job.last_event.as_deref(),
            Some("worker_launching") | Some("worker_spawned") | None
        )
        && job.session_id.is_none()
        && job.turn_id.is_none()
}

fn build_worker_boot_failure_message(
    job_store: &HostJobStore,
    job_id: Uuid,
    status: std::process::ExitStatus,
) -> String {
    let mut message = format!(
        "host worker exited before initialization (exit code: {:?})",
        status.code()
    );

    if let Some(stderr_tail) =
        read_log_tail(&job_store.worker_stderr_path(job_id), WORKER_LOG_TAIL_BYTES)
    {
        let stderr_tail = collapse_whitespace(&stderr_tail);
        if !stderr_tail.is_empty() {
            message.push_str(&format!(
                "; stderr: {stderr_tail}; stderr_log={}",
                job_store.worker_stderr_path(job_id).display()
            ));
            return message;
        }
    }

    if let Some(stdout_tail) =
        read_log_tail(&job_store.worker_stdout_path(job_id), WORKER_LOG_TAIL_BYTES)
    {
        let stdout_tail = collapse_whitespace(&stdout_tail);
        if !stdout_tail.is_empty() {
            message.push_str(&format!(
                "; stdout: {stdout_tail}; stdout_log={}",
                job_store.worker_stdout_path(job_id).display()
            ));
        }
    }

    message
}

fn read_log_tail(path: &Path, max_bytes: usize) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    if data.is_empty() {
        return None;
    }

    let start = data.len().saturating_sub(max_bytes);
    let tail = String::from_utf8_lossy(&data[start..]).trim().to_string();
    (!tail.is_empty()).then_some(tail)
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn emit_wait_result(
    job_store: &HostJobStore,
    job_id: Uuid,
    wait_secs: u64,
    command: &'static str,
) {
    let job = wait_for_job(job_store, job_id, wait_secs).await;
    let job = match job {
        Ok(job) => job,
        Err(err) => {
            print_error(
                "execution_failed",
                &format!("wait for job '{job_id}': {err}"),
            );
            process::exit(1);
        }
    };

    if job.status.is_terminal() {
        emit_json(&job_bridge_json(command, &job));
        return;
    }

    let timed_out = match job_store.update(job_id, |job| job.touch_wait_timeout()) {
        Ok(job) => job,
        Err(err) => {
            print_error(
                "execution_failed",
                &format!("update wait-timeout state for job '{job_id}': {err}"),
            );
            process::exit(1);
        }
    };
    emit_json(&wait_timeout_bridge_json(command, &timed_out));
}

async fn wait_for_job(
    job_store: &HostJobStore,
    job_id: Uuid,
    wait_secs: u64,
) -> Result<HostJobState, std::io::Error> {
    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    loop {
        let job = load_job_with_refresh(job_store, job_id)?
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "job not found"))?;
        if job.status.is_terminal() || wait_secs == 0 || Instant::now() >= deadline {
            return Ok(job);
        }
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

struct PreparedHostJobRun {
    provider_name: String,
    provider_impl: Box<dyn Provider>,
    full_message: String,
    config: SwitchyardConfig,
    cwd: PathBuf,
    artifact_dir: PathBuf,
}

async fn prepare_host_job_run(
    registry: &ProviderRegistry,
    config: &SwitchyardConfig,
    provider_name: &str,
    task: &str,
    cwd: &Path,
) -> Result<PreparedHostJobRun, String> {
    let provider_impl = registry
        .create(provider_name, config.providers.get(provider_name))
        .ok_or_else(|| format!("provider '{provider_name}' not registered"))?;

    let peer_catalog = build_peer_catalog_probed(provider_name, registry, &config.providers).await;
    let hyard_guidance = peer_catalog.render_prompt(PromptMode::Hyard);
    let full_message = if peer_catalog.peers.is_empty() {
        task.to_string()
    } else {
        format!("{task}\n\n---\n{hyard_guidance}")
    };

    Ok(PreparedHostJobRun {
        provider_name: provider_name.to_string(),
        provider_impl,
        full_message,
        config: config.clone(),
        cwd: cwd.to_path_buf(),
        artifact_dir: config.artifact_dir(cwd),
    })
}

async fn run_host_job(
    job_store: HostJobStore,
    job_id: Uuid,
    prepared: PreparedHostJobRun,
) -> Result<(), String> {
    let preflight_job = job_store
        .load(job_id)
        .map_err(|err| format!("load job '{job_id}': {err}"))?
        .ok_or_else(|| format!("job '{job_id}' not found"))?;

    if preflight_job.status == HostJobStatus::CancelRequested {
        job_store
            .update(job_id, |job| {
                job.status = HostJobStatus::Cancelled;
                job.completed_at = Some(Utc::now());
                job.last_event = Some("cancelled_before_start".to_string());
                job.error = Some("cancelled".to_string());
            })
            .map_err(|err| format!("cancel job '{job_id}' before start: {err}"))?;
        return Ok(());
    }

    let mut store = StoreHandle::open(
        prepared.config.store_backend(),
        prepared.config.store_path(&prepared.cwd),
    )
    .map_err(|err| format!("open store: {err}"))?;
    let mut session = Session::new(prepared.provider_name.clone());
    store
        .save_session(&session)
        .map_err(|err| format!("session init: {err}"))?;

    job_store
        .update(job_id, |job| {
            job.status = HostJobStatus::Running;
            if job.started_at.is_none() {
                job.started_at = Some(Utc::now());
            }
            job.session_id = Some(session.session_id);
            job.last_event = Some("worker_started".to_string());
        })
        .map_err(|err| format!("mark job running: {err}"))?;

    let (runtime_tx, mut runtime_rx) = mpsc::channel(128);
    let cancel = CancellationToken::new();

    let event_store = job_store.clone();
    let event_job_id = job_id;
    let event_task = tokio::spawn(async move {
        while let Some(event) = runtime_rx.recv().await {
            let _ = event_store.update(event_job_id, |job| apply_runtime_event(job, &event));
        }
    });

    let cancel_store = job_store.clone();
    let cancel_job_id = job_id;
    let cancel_token = cancel.clone();
    let cancel_task = tokio::spawn(async move {
        while let Ok(Some(job)) = cancel_store.load(cancel_job_id) {
            if job.status.is_terminal() {
                break;
            }
            if job.status == HostJobStatus::CancelRequested {
                cancel_token.cancel();
                break;
            }

            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
        }
    });

    let run_result = run_turn_phased(
        &mut store,
        &mut session,
        prepared.provider_impl.as_ref(),
        prepared.full_message,
        prepared.cwd.clone(),
        Some(&prepared.artifact_dir),
        Some(&runtime_tx),
        TurnPhase::Normal,
        cancel.clone(),
    )
    .await;

    drop(runtime_tx);
    let _ = event_task.await;
    cancel_task.abort();

    match run_result {
        Ok(output) => {
            let turns = store
                .list_turns(session.session_id)
                .map_err(|err| format!("load turns for job '{job_id}': {err}"))?;
            let final_turn = turns
                .into_iter()
                .find(|turn| turn.turn_id == output.turn_id)
                .ok_or_else(|| format!("final turn '{}' not found", output.turn_id))?;
            let artifacts = store.list_artifacts(output.turn_id).unwrap_or_default();

            job_store
                .update(job_id, |job| {
                    let preview_text = final_turn
                        .provider_response
                        .clone()
                        .or_else(|| job.last_output_preview.clone());
                    job.turn_id = Some(output.turn_id);
                    job.completed_at = Some(Utc::now());
                    job.result_summary = final_turn.provider_response.clone();
                    job.artifact_count = artifacts.len();
                    job.error = final_turn.error_message.clone();
                    job.set_last_preview(preview_text.as_deref());
                    job.result_ready = matches!(final_turn.status, TurnStatus::Completed);
                    job.status = match final_turn.status {
                        TurnStatus::Completed => HostJobStatus::Completed,
                        TurnStatus::Cancelled => HostJobStatus::Cancelled,
                        TurnStatus::Failed if cancel.is_cancelled() => HostJobStatus::Cancelled,
                        TurnStatus::Failed
                            if final_turn.error_message.as_deref() == Some("cancelled") =>
                        {
                            HostJobStatus::Cancelled
                        }
                        _ => HostJobStatus::Failed,
                    };
                    job.last_event = Some(match job.status {
                        HostJobStatus::Completed => "turn_completed".to_string(),
                        HostJobStatus::Cancelled => "turn_cancelled".to_string(),
                        HostJobStatus::Failed => "turn_failed".to_string(),
                        _ => "turn_finished".to_string(),
                    });
                })
                .map_err(|err| format!("write final job state '{job_id}': {err}"))?;
            Ok(())
        }
        Err(err) => {
            job_store
                .update(job_id, |job| {
                    job.status = if cancel.is_cancelled() {
                        HostJobStatus::Cancelled
                    } else {
                        HostJobStatus::Failed
                    };
                    job.completed_at = Some(Utc::now());
                    job.error = Some(err.to_string());
                    job.last_event = Some(if cancel.is_cancelled() {
                        "turn_cancelled".to_string()
                    } else {
                        "turn_failed".to_string()
                    });
                })
                .map_err(|write_err| format!("write failed job state '{job_id}': {write_err}"))?;
            Ok(())
        }
    }
}

fn apply_runtime_event(job: &mut HostJobState, event: &RuntimeEvent) {
    match event {
        RuntimeEvent::CoreTurnStarted { turn_id, provider } => {
            job.status = HostJobStatus::Running;
            job.turn_id = Some(*turn_id);
            job.last_event = Some(format!("turn_started:{provider}"));
        }
        RuntimeEvent::CoreExecutionTelemetry {
            turn_id,
            provider,
            execution,
        } => {
            job.status = HostJobStatus::Running;
            job.turn_id = Some(*turn_id);
            job.execution = Some(execution.clone());
            job.last_event = Some(format!("execution_resolved:{provider}"));
        }
        RuntimeEvent::CoreItemUpdated {
            turn_id,
            provider,
            text,
        } => {
            job.status = HostJobStatus::Running;
            job.turn_id = Some(*turn_id);
            job.last_event = Some(format!("item_updated:{provider}"));
            job.set_last_preview(Some(text));
        }
        RuntimeEvent::CoreTerminalOutput {
            turn_id,
            provider,
            text,
            ..
        } => {
            job.status = HostJobStatus::Running;
            job.turn_id = Some(*turn_id);
            job.last_event = Some(format!("terminal_output:{provider}"));
            job.set_last_preview(Some(text));
        }
        RuntimeEvent::CoreOutputCompleted { turn_id, provider } => {
            job.turn_id = Some(*turn_id);
            job.last_event = Some(format!("output_completed:{provider}"));
        }
        RuntimeEvent::TurnCompleted {
            turn_id,
            provider,
            response,
        } => {
            job.turn_id = Some(*turn_id);
            job.last_event = Some(format!("turn_completed:{provider}"));
            job.set_last_preview(response.as_deref());
        }
        RuntimeEvent::TurnFailed {
            turn_id,
            provider,
            error,
        } => {
            job.turn_id = Some(*turn_id);
            job.last_event = Some(format!("turn_failed:{provider}"));
            if !error.trim().is_empty() {
                job.error = Some(error.clone());
            }
        }
        RuntimeEvent::DelegateRequested { .. }
        | RuntimeEvent::HyardJobObserved { .. }
        | RuntimeEvent::PeerExecutionTelemetry { .. }
        | RuntimeEvent::PeerTurnStarted { .. }
        | RuntimeEvent::PeerItemUpdated { .. }
        | RuntimeEvent::PeerTerminalOutput { .. }
        | RuntimeEvent::DelegateCompleted { .. }
        | RuntimeEvent::PeerOutputCompleted { .. }
        | RuntimeEvent::FinalizationStarted { .. } => {}
    }
}

fn legacy_turn_json(job_id: &str, turn: &switchyard_session::Turn) -> serde_json::Value {
    serde_json::json!({
        "job_id": job_id,
        "status": turn.status.to_string(),
        "provider": turn.provider,
        "session_id": turn.session_id.to_string(),
        "turn_id": turn.turn_id.to_string(),
        "last_event": serde_json::Value::Null,
        "last_output_preview": turn.provider_response,
        "summary": turn.provider_response,
        "artifact_count": 0,
        "result_ready": matches!(turn.status, TurnStatus::Completed),
        "wait_timeout_count": 0,
        "error": turn.error_message,
    })
}

fn mark_job_failed(job_store: &HostJobStore, job_id: Uuid, message: &str) {
    let _ = job_store.update(job_id, |job| {
        job.status = HostJobStatus::Failed;
        job.completed_at = Some(Utc::now());
        job.error = Some(message.to_string());
        job.last_event = Some("worker_failed".to_string());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_orphaned_job_marks_missing_worker_failed() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostJobStore::new(dir.path().to_path_buf());
        let mut job = HostJobState::new("claude", "task", PathBuf::from("."));
        job.status = HostJobStatus::Running;
        job.pid = Some(4242);
        job.last_event = Some("worker_started".to_string());
        store.save(&job).unwrap();
        std::fs::write(
            store.worker_stderr_path(job.job_id),
            "spawn failed: access denied",
        )
        .unwrap();

        let refreshed = refresh_orphaned_job_state_with(&store, job.job_id, |_| false)
            .unwrap()
            .unwrap();

        assert_eq!(refreshed.status, HostJobStatus::Failed);
        assert_eq!(refreshed.last_event.as_deref(), Some("worker_missing"));
        assert!(
            refreshed
                .error
                .as_deref()
                .is_some_and(|msg| msg.contains("access denied"))
        );
    }

    #[test]
    fn boot_failure_message_includes_log_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostJobStore::new(dir.path().to_path_buf());
        let job = HostJobState::new("claude", "task", PathBuf::from("."));
        store.save(&job).unwrap();
        std::fs::write(
            store.worker_stderr_path(job.job_id),
            "spawn failed: access denied",
        )
        .unwrap();

        let message = build_worker_boot_failure_message(
            &store,
            job.job_id,
            std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--help")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap(),
        );

        assert!(message.contains("access denied"));
        assert!(message.contains("stderr_log="));
    }

    #[test]
    fn worker_boot_stall_detection_only_matches_uninitialized_jobs() {
        let mut queued = HostJobState::new("claude", "task", PathBuf::from("."));
        queued.last_event = Some("worker_spawned".to_string());
        assert!(is_stalled_at_worker_boot(&queued));

        let mut progressed = HostJobState::new("claude", "task", PathBuf::from("."));
        progressed.status = HostJobStatus::Running;
        progressed.last_event = Some("worker_booting".to_string());
        assert!(!is_stalled_at_worker_boot(&progressed));

        let mut failed = HostJobState::new("claude", "task", PathBuf::from("."));
        failed.status = HostJobStatus::Failed;
        failed.last_event = Some("turn_failed".to_string());
        assert!(!is_stalled_at_worker_boot(&failed));
    }

    #[test]
    fn wait_timeout_bridge_response_includes_protocol_message_and_next_actions() {
        let mut job = HostJobState::new("claude", "task", PathBuf::from("."));
        job.status = HostJobStatus::Running;
        job.touch_wait_timeout();

        let json = wait_timeout_bridge_json("delegate", &job);

        assert_eq!(json["protocol"], HYARD_PROTOCOL);
        assert_eq!(json["command"], "delegate");
        assert_eq!(json["status"], "wait_timeout");
        assert!(
            json["message"]
                .as_str()
                .is_some_and(|message| message.contains("same job_id"))
        );
        let next_actions = json["next_actions"].as_array().cloned().unwrap_or_default();
        assert!(next_actions.contains(&serde_json::json!("status")));
        assert!(next_actions.contains(&serde_json::json!("result")));
        assert!(next_actions.contains(&serde_json::json!("await")));
    }

    #[test]
    fn result_bridge_response_marks_running_job_as_not_ready() {
        let mut job = HostJobState::new("gemini", "task", PathBuf::from("."));
        job.status = HostJobStatus::Running;
        job.result_ready = false;

        let json = job_bridge_json("result", &job);

        assert_eq!(json["protocol"], HYARD_PROTOCOL);
        assert_eq!(json["command"], "result");
        assert_eq!(json["status"], "running");
        assert!(
            json["message"]
                .as_str()
                .is_some_and(|message| message.contains("result is not ready yet"))
        );
        let next_actions = json["next_actions"].as_array().cloned().unwrap_or_default();
        assert!(next_actions.contains(&serde_json::json!("status")));
        assert!(next_actions.contains(&serde_json::json!("cancel")));
    }
}
