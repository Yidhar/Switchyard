//! `switchyard host` subcommand — machine-readable bridge for host packs.
//!
//! All machine-readable output is emitted as a single JSON document on stdout.
//! Exit 0 = success, non-zero = error.
//! Host packs (Claude/Gemini/Codex) call these subcommands to implement /hyard:*.

#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
#[cfg(windows)]
use std::{
    ffi::{OsStr, OsString},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    },
    ptr,
};
use std::{io::Write, process};

use chrono::Utc;
use switchyard_config::SwitchyardConfig;
use switchyard_core::{
    ProviderRegistry, RuntimeEvent, TurnPhase, build_peer_catalog_probed,
    execution_policy_from_config, run_routed_turn_with_archive_and_policy,
    run_turn_phased_with_policy,
};
#[cfg(test)]
use switchyard_host_jobs::refresh_orphaned_job_state_with;
use switchyard_host_jobs::{
    HostJobState, HostJobStatus, HostJobStore, HostJobWorkerMode, load_job_with_refresh,
};
use switchyard_provider_api::{CancellationToken, PromptMode, Provider};
use switchyard_session::{InboxDeliveryMode, InboxEntry, InboxStatus, Session, TurnStatus};
use switchyard_store::{
    ArtifactStore, SessionCatalog, SessionInboxRepository, SessionRepository, StoreHandle,
    TurnRepository,
};
use tokio::sync::mpsc;
use uuid::Uuid;
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE},
    System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
        InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION,
        STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
    },
};

pub const DEFAULT_WAIT_SECS: u64 = 1;
const HYARD_PROTOCOL: &str = "hyard_v2";

const POLL_INTERVAL_MS: u64 = 200;
const INLINE_WORKER_ENV: &str = "SWITCHYARD_HOST_WORKER_INLINE";
const WORKER_BOOT_TIMEOUT_MS: u64 = 1_500;
const WORKER_BOOT_POLL_MS: u64 = 50;
const WORKER_LOG_TAIL_BYTES: usize = 8 * 1024;
const CALLBACK_RESUME_MESSAGE: &str = "Background callback receipts are ready. Continue this existing session, absorb any injected callback results, and proceed with the user's task from the latest state.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostFollowMode {
    Once,
    Resident,
}

#[derive(Debug, Clone, Copy, Default)]
struct BridgeTimingMs {
    launch_ms: u64,
    wait_ms: u64,
    total_ms: u64,
}

#[derive(Debug, Clone)]
struct HostResumeOutcome {
    status: &'static str,
    session_id: Uuid,
    provider: String,
    turn_id: Option<Uuid>,
    delegated: bool,
    response: Option<String>,
    resume_reason: &'static str,
    input_message: String,
    unread_count_before: usize,
    unread_count_after: usize,
    unread_callback_count_before: usize,
    unread_callback_count_after: usize,
    delivered_callback_count: usize,
    resumable: bool,
    callback_pending: bool,
    message: String,
}

impl HostResumeOutcome {
    fn to_json(&self, command: &'static str) -> serde_json::Value {
        serde_json::json!({
            "protocol": HYARD_PROTOCOL,
            "command": command,
            "status": self.status,
            "session_id": self.session_id.to_string(),
            "provider": &self.provider,
            "turn_id": self.turn_id.map(|id| id.to_string()),
            "delegated": self.delegated,
            "response": &self.response,
            "resume_reason": self.resume_reason,
            "input_message": &self.input_message,
            "unread_count_before": self.unread_count_before,
            "unread_count_after": self.unread_count_after,
            "unread_callback_count_before": self.unread_callback_count_before,
            "unread_callback_count_after": self.unread_callback_count_after,
            "delivered_callback_count": self.delivered_callback_count,
            "resumable": self.resumable,
            "callback_pending": self.callback_pending,
            "message": &self.message,
            "next_actions": if matches!(self.status, "noop" | "busy") {
                serde_json::json!(["watch", "follow", "inbox"])
            } else {
                serde_json::json!(["inbox", "watch", "follow", "resume"])
            },
        })
    }
}

#[derive(Debug, Clone)]
struct HostFollowWatchOutcome {
    status: &'static str,
    session_id: Uuid,
    timeout_sec: u64,
    unread_count: usize,
    unread_callback_count: usize,
    items: Vec<InboxEntry>,
    message: String,
}

impl HostFollowWatchOutcome {
    fn callback_pending(&self) -> bool {
        self.unread_callback_count > 0
    }

    fn returned_count(&self) -> usize {
        self.items.len()
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": self.status,
            "session_id": self.session_id.to_string(),
            "timeout_sec": self.timeout_sec,
            "unread_count": self.unread_count,
            "unread_count_after": self.unread_count,
            "unread_callback_count": self.unread_callback_count,
            "unread_callback_count_after": self.unread_callback_count,
            "returned_count": self.returned_count(),
            "callback_pending": self.callback_pending(),
            "message": &self.message,
            "items": self.items.iter().map(inbox_entry_json).collect::<Vec<_>>(),
        })
    }
}

fn open_configured_store(config: &SwitchyardConfig, cwd: &Path) -> Result<StoreHandle, String> {
    StoreHandle::open(config.store_backend(cwd), config.store_path(cwd))
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
        None,
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
    callback_session_selector: Option<&str>,
) {
    let total_start = Instant::now();
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
    if let Some(selector) = callback_session_selector {
        let store = open_configured_store(config, cwd).unwrap_or_else(|err| {
            print_error("execution_failed", &err);
            std::process::exit(1);
        });
        let callback_session_id =
            resolve_host_session_selector(&store, selector).unwrap_or_else(|err| {
                print_error("not_found", &err);
                std::process::exit(1);
            });
        job.callback_session_id = Some(callback_session_id);
    }
    job.worker_mode = Some(if should_run_worker_inline() {
        HostJobWorkerMode::Inline
    } else {
        HostJobWorkerMode::Subprocess
    });
    if let Err(err) = job_store.save(&job) {
        print_error("execution_failed", &format!("job init: {err}"));
        std::process::exit(1);
    }

    let launch_start = Instant::now();
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

    emit_wait_result(
        &job_store,
        job.job_id,
        wait_secs,
        "delegate",
        BridgeTimingMs {
            launch_ms: clamp_millis_u64(launch_start.elapsed()),
            ..BridgeTimingMs::default()
        },
        total_start,
    )
    .await;
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
    let total_start = Instant::now();
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

    emit_wait_result(
        &job_store,
        parsed_job_id,
        timeout_secs,
        "await",
        BridgeTimingMs::default(),
        total_start,
    )
    .await;
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

/// Execute `switchyard host inbox` — read callback receipts from a session inbox.
pub async fn host_inbox(
    config: &SwitchyardConfig,
    session_selector: Option<&str>,
    resume_latest: bool,
    all: bool,
    mark_read: bool,
    consume: bool,
    cwd: &Path,
) {
    let mut store = open_configured_store(config, cwd).unwrap_or_else(|err| {
        print_error("execution_failed", &err);
        process::exit(1);
    });

    let session_id = resolve_host_inbox_session_id(&store, session_selector, resume_latest)
        .unwrap_or_else(|err| {
            print_error("not_found", &err);
            process::exit(1);
        });

    let items = load_sorted_inbox_entries(&store, session_id).unwrap_or_else(|err| {
        print_error("execution_failed", &err);
        process::exit(1);
    });
    let unread_count_before = items.iter().filter(|entry| entry.is_unread()).count();
    let mut returned = if all {
        items
    } else {
        items
            .into_iter()
            .filter(|entry| entry.is_unread())
            .collect()
    };

    apply_inbox_mutations(&mut store, &mut returned, mark_read, consume).unwrap_or_else(|err| {
        print_error("execution_failed", &err);
        process::exit(1);
    });

    let returned_count = returned.len();
    let unread_count = count_unread_inbox_entries(&store, session_id).unwrap_or_else(|err| {
        print_error("execution_failed", &err);
        process::exit(1);
    });
    let command = "inbox";
    let message = if returned_count == 0 {
        if all {
            format!("session '{session_id}' inbox is empty")
        } else {
            format!("session '{session_id}' has no unread callback receipts")
        }
    } else if consume {
        format!(
            "returned and consumed {returned_count} callback receipt(s) for session '{session_id}'"
        )
    } else if mark_read {
        format!(
            "returned and marked {returned_count} callback receipt(s) as read for session '{session_id}'"
        )
    } else {
        format!("returned {returned_count} callback receipt(s) for session '{session_id}'")
    };

    emit_json(&serde_json::json!({
        "protocol": HYARD_PROTOCOL,
        "command": command,
        "status": if returned_count == 0 { "empty" } else { "ok" },
        "session_id": session_id.to_string(),
        "unread_count": unread_count,
        "unread_count_before": unread_count_before,
        "returned_count": returned_count,
        "callback_pending": unread_count > 0,
        "all": all,
        "mark_read": mark_read,
        "consume": consume,
        "message": message,
        "items": returned.iter().map(inbox_entry_json).collect::<Vec<_>>(),
    }));
}

/// Execute `switchyard host watch` — wait for callback receipts to arrive in a session inbox.
pub async fn host_watch(
    config: &SwitchyardConfig,
    session_selector: Option<&str>,
    resume_latest: bool,
    timeout_secs: u64,
    mark_read: bool,
    consume: bool,
    cwd: &Path,
) {
    let mut store = open_configured_store(config, cwd).unwrap_or_else(|err| {
        print_error("execution_failed", &err);
        process::exit(1);
    });

    let session_id = resolve_host_inbox_session_id(&store, session_selector, resume_latest)
        .unwrap_or_else(|err| {
            print_error("not_found", &err);
            process::exit(1);
        });

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let items = load_sorted_inbox_entries(&store, session_id).unwrap_or_else(|err| {
            print_error("execution_failed", &err);
            process::exit(1);
        });
        let unread_count_before = items.iter().filter(|entry| entry.is_unread()).count();
        let mut returned: Vec<InboxEntry> = items
            .into_iter()
            .filter(|entry| entry.is_unread())
            .collect();

        if !returned.is_empty() {
            apply_inbox_mutations(&mut store, &mut returned, mark_read, consume).unwrap_or_else(
                |err| {
                    print_error("execution_failed", &err);
                    process::exit(1);
                },
            );
            let unread_count =
                count_unread_inbox_entries(&store, session_id).unwrap_or_else(|err| {
                    print_error("execution_failed", &err);
                    process::exit(1);
                });
            let returned_count = returned.len();
            let message = if consume {
                format!(
                    "received and consumed {returned_count} callback receipt(s) for session '{session_id}'"
                )
            } else if mark_read {
                format!(
                    "received and marked {returned_count} callback receipt(s) as read for session '{session_id}'"
                )
            } else {
                format!("received {returned_count} callback receipt(s) for session '{session_id}'")
            };
            emit_json(&serde_json::json!({
                "protocol": HYARD_PROTOCOL,
                "command": "watch",
                "status": "callback_ready",
                "session_id": session_id.to_string(),
                "timeout_sec": timeout_secs,
                "unread_count": unread_count,
                "unread_count_after": unread_count,
                "unread_count_before": unread_count_before,
                "returned_count": returned_count,
                "callback_pending": unread_count > 0,
                "mark_read": mark_read,
                "consume": consume,
                "message": message,
                "next_actions": ["resume", "inbox", "result"],
                "items": returned.iter().map(inbox_entry_json).collect::<Vec<_>>(),
            }));
            return;
        }

        if Instant::now() >= deadline {
            emit_json(&serde_json::json!({
                "protocol": HYARD_PROTOCOL,
                "command": "watch",
                "status": "wait_timeout",
                "session_id": session_id.to_string(),
                "timeout_sec": timeout_secs,
                "unread_count": 0,
                "unread_count_after": 0,
                "unread_count_before": unread_count_before,
                "returned_count": 0,
                "callback_pending": false,
                "mark_read": mark_read,
                "consume": consume,
                "message": format!(
                    "no callback receipts arrived for session '{session_id}' during this watch window; continue other work and re-arm watch later"
                ),
                "next_actions": ["watch", "inbox", "resume"],
                "items": Vec::<serde_json::Value>::new(),
            }));
            return;
        }

        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

/// Execute `switchyard host resume` — continue an existing session with either
/// a direct user message or unread callback receipts as the resume trigger.
pub async fn host_resume(
    registry: &ProviderRegistry,
    config: &SwitchyardConfig,
    session_selector: Option<&str>,
    resume_latest: bool,
    message: Option<&str>,
    callbacks: bool,
    cwd: &Path,
) {
    let total_start = Instant::now();
    let store = open_configured_store(config, cwd).unwrap_or_else(|err| {
        print_error("execution_failed", &err);
        process::exit(1);
    });

    let session_id = resolve_host_inbox_session_id(&store, session_selector, resume_latest)
        .unwrap_or_else(|err| {
            print_error("not_found", &err);
            process::exit(1);
        });

    let outcome =
        execute_host_resume_for_session(registry, config, session_id, message, callbacks, cwd)
            .await
            .unwrap_or_else(|err| {
                let code = if err.starts_with("provider_unavailable:") {
                    "provider_unavailable"
                } else if err.starts_with("invalid_arguments:") {
                    "invalid_arguments"
                } else if err.starts_with("not_found:") {
                    "not_found"
                } else {
                    "execution_failed"
                };
                let detail = err
                    .split_once(':')
                    .map(|(_, detail)| detail.trim())
                    .unwrap_or(err.as_str());
                print_error(code, detail);
                process::exit(1);
            });

    emit_json(&attach_timing(
        outcome.to_json("resume"),
        BridgeTimingMs {
            total_ms: clamp_millis_u64(total_start.elapsed()),
            ..BridgeTimingMs::default()
        },
    ));
}

/// Execute `switchyard host follow` — wait for resumable callback receipts and
/// automatically continue the same session when they arrive.
///
/// With `resident = true`, this becomes a long-lived callback consumer that
/// keeps re-arming the same watch->resume flow until the process is stopped.
pub async fn host_follow(
    registry: &ProviderRegistry,
    config: &SwitchyardConfig,
    session_selector: Option<&str>,
    resume_latest: bool,
    timeout_secs: u64,
    resident: bool,
    cwd: &Path,
) {
    let store = open_configured_store(config, cwd).unwrap_or_else(|err| {
        print_error("execution_failed", &err);
        process::exit(1);
    });

    let session_id = resolve_host_inbox_session_id(&store, session_selector, resume_latest)
        .unwrap_or_else(|err| {
            print_error("not_found", &err);
            process::exit(1);
        });

    let mode = if resident {
        HostFollowMode::Resident
    } else {
        HostFollowMode::Once
    };
    let mut cycle = 0_u64;

    loop {
        cycle += 1;
        let cycle_start = Instant::now();
        let event = execute_host_follow_cycle(registry, config, session_id, timeout_secs, cwd)
            .await
            .unwrap_or_else(|err| {
                let code = if err.starts_with("provider_unavailable:") {
                    "provider_unavailable"
                } else if err.starts_with("invalid_arguments:") {
                    "invalid_arguments"
                } else if err.starts_with("not_found:") {
                    "not_found"
                } else {
                    "execution_failed"
                };
                let detail = err
                    .split_once(':')
                    .map(|(_, detail)| detail.trim())
                    .unwrap_or(err.as_str());
                print_error(code, detail);
                process::exit(1);
            });

        let mut event = attach_timing(
            event,
            BridgeTimingMs {
                total_ms: clamp_millis_u64(cycle_start.elapsed()),
                ..BridgeTimingMs::default()
            },
        );
        if mode == HostFollowMode::Resident
            && let Some(obj) = event.as_object_mut()
        {
            obj.insert("resident".to_string(), serde_json::json!(true));
            obj.insert("cycle".to_string(), serde_json::json!(cycle));
            obj.insert(
                "message".to_string(),
                serde_json::json!(
                    obj.get("message")
                        .and_then(|value| value.as_str())
                        .map(|message| format!("{message} (resident cycle #{cycle})"))
                        .unwrap_or_else(|| format!("resident follow cycle #{cycle}"))
                ),
            );
        }
        emit_json(&event);

        if mode == HostFollowMode::Once {
            return;
        }
    }
}

async fn execute_host_follow_cycle(
    registry: &ProviderRegistry,
    config: &SwitchyardConfig,
    session_id: Uuid,
    timeout_secs: u64,
    cwd: &Path,
) -> Result<serde_json::Value, String> {
    let store = open_configured_store(config, cwd)?;
    let session = store
        .load_session(session_id)
        .map_err(|err| format!("execution_failed: load session '{session_id}': {err}"))?
        .ok_or_else(|| format!("not_found: session '{session_id}' not found in selected store"))?;

    let watch = wait_for_resumable_callback_receipts(config, session_id, timeout_secs, cwd).await?;
    if watch.status == "wait_timeout" {
        return Ok(follow_wait_timeout_json(
            session_id,
            &session.active_core,
            timeout_secs,
            &watch,
        ));
    }

    let resume =
        execute_host_resume_for_session(registry, config, session_id, None, true, cwd).await?;
    Ok(follow_resumed_json(
        session_id,
        timeout_secs,
        &watch,
        &resume,
    ))
}

fn follow_wait_timeout_json(
    session_id: Uuid,
    provider: &str,
    timeout_secs: u64,
    watch: &HostFollowWatchOutcome,
) -> serde_json::Value {
    serde_json::json!({
        "protocol": HYARD_PROTOCOL,
        "command": "follow",
        "status": "wait_timeout",
        "session_id": session_id.to_string(),
        "provider": provider,
        "timeout_sec": timeout_secs,
        "watch_status": watch.status,
        "resume_attempted": false,
        "resume_status": serde_json::Value::Null,
        "turn_id": serde_json::Value::Null,
        "delegated": false,
        "response": serde_json::Value::Null,
        "resume_reason": "callbacks",
        "input_message": CALLBACK_RESUME_MESSAGE,
        "unread_count_before": watch.unread_count,
        "unread_count_after": watch.unread_count,
        "unread_callback_count_before": watch.unread_callback_count,
        "unread_callback_count_after": watch.unread_callback_count,
        "delivered_callback_count": 0,
        "resumable": false,
        "callback_pending": watch.callback_pending(),
        "message": &watch.message,
        "next_actions": ["follow", "watch", "inbox", "resume"],
        "watch": watch.to_json(),
        "resume": serde_json::Value::Null,
    })
}

fn follow_resumed_json(
    session_id: Uuid,
    timeout_secs: u64,
    watch: &HostFollowWatchOutcome,
    resume: &HostResumeOutcome,
) -> serde_json::Value {
    let follow_message = if resume.status == "completed" {
        format!(
            "callback receipts became ready for session '{session_id}' and follow resumed the existing session on provider '{}'",
            resume.provider
        )
    } else if resume.status == "busy" {
        format!(
            "callback receipts are ready for session '{session_id}', but follow detected another active turn lease and did not start a concurrent resume"
        )
    } else {
        format!(
            "callback receipts briefly became ready for session '{session_id}', but follow found nothing resumable by the time resume executed"
        )
    };

    serde_json::json!({
        "protocol": HYARD_PROTOCOL,
        "command": "follow",
        "status": resume.status,
        "session_id": session_id.to_string(),
        "provider": &resume.provider,
        "timeout_sec": timeout_secs,
        "watch_status": watch.status,
        "resume_attempted": true,
        "resume_status": resume.status,
        "turn_id": resume.turn_id.map(|id| id.to_string()),
        "delegated": resume.delegated,
        "response": &resume.response,
        "resume_reason": resume.resume_reason,
        "input_message": &resume.input_message,
        "unread_count_before": resume.unread_count_before,
        "unread_count_after": resume.unread_count_after,
        "unread_callback_count_before": resume.unread_callback_count_before,
        "unread_callback_count_after": resume.unread_callback_count_after,
        "delivered_callback_count": resume.delivered_callback_count,
        "resumable": resume.resumable,
        "callback_pending": resume.callback_pending,
        "message": follow_message,
        "next_actions": if resume.status == "completed" {
            serde_json::json!(["inbox", "watch", "follow", "resume"])
        } else {
            serde_json::json!(["follow", "watch", "inbox"])
        },
        "watch": watch.to_json(),
        "resume": resume.to_json("resume"),
    })
}

async fn execute_host_resume_for_session(
    registry: &ProviderRegistry,
    config: &SwitchyardConfig,
    session_id: Uuid,
    message: Option<&str>,
    callbacks: bool,
    cwd: &Path,
) -> Result<HostResumeOutcome, String> {
    let mut store = open_configured_store(config, cwd)?;
    let mut session = store
        .load_session(session_id)
        .map_err(|err| format!("execution_failed: load session '{session_id}': {err}"))?
        .ok_or_else(|| format!("not_found: session '{session_id}' not found in selected store"))?;

    let unread_count_before = count_unread_inbox_entries(&store, session_id)?;
    let unread_callback_count_before = count_resumable_callback_entries(&store, session_id)?;
    let provider_name = session.active_core.clone();
    let resume_reason = if callbacks { "callbacks" } else { "message" };
    if session.active_turn_is_live() {
        return Ok(HostResumeOutcome {
            status: "busy",
            session_id,
            provider: provider_name,
            turn_id: session.active_turn_id,
            delegated: false,
            response: None,
            resume_reason,
            input_message: message.unwrap_or(CALLBACK_RESUME_MESSAGE).to_string(),
            unread_count_before,
            unread_count_after: unread_count_before,
            unread_callback_count_before,
            unread_callback_count_after: unread_callback_count_before,
            delivered_callback_count: 0,
            resumable: false,
            callback_pending: unread_callback_count_before > 0,
            message: format!(
                "session '{session_id}' already has an active turn lease{}; wait for it to finish or let follow/watch keep listening",
                session
                    .active_turn_id
                    .map(|turn_id| format!(" (turn {turn_id})"))
                    .unwrap_or_default()
            ),
        });
    }

    let user_message = if callbacks {
        if unread_callback_count_before == 0 {
            return Ok(HostResumeOutcome {
                status: "noop",
                session_id,
                provider: provider_name,
                turn_id: None,
                delegated: false,
                response: None,
                resume_reason,
                input_message: CALLBACK_RESUME_MESSAGE.to_string(),
                unread_count_before,
                unread_count_after: unread_count_before,
                unread_callback_count_before,
                unread_callback_count_after: unread_callback_count_before,
                delivered_callback_count: 0,
                resumable: false,
                callback_pending: false,
                message: format!(
                    "session '{session_id}' has no unread callback receipts eligible for resume injection"
                ),
            });
        }
        CALLBACK_RESUME_MESSAGE.to_string()
    } else {
        message
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "invalid_arguments: `--message` cannot be empty".to_string())?
            .to_string()
    };

    let provider_impl = registry
        .create(&provider_name, config.providers.get(&provider_name))
        .ok_or_else(|| {
            format!(
                "provider_unavailable: provider '{provider_name}' is not registered for session '{session_id}'"
            )
        })?;

    let peer_catalog = build_peer_catalog_probed(&provider_name, registry, &config.providers).await;
    let artifact_dir = config.artifact_dir(cwd);
    let policy = execution_policy_from_config(config, cwd);
    let output = run_routed_turn_with_archive_and_policy(
        &mut store,
        &mut session,
        provider_impl.as_ref(),
        &peer_catalog,
        &|name| registry.create(name, config.providers.get(name)),
        None,
        user_message.clone(),
        cwd.to_path_buf(),
        Some(&artifact_dir),
        policy,
    )
    .await
    .map_err(|err| {
        format!(
            "execution_failed: resume session '{session_id}' on provider '{provider_name}': {err}"
        )
    })?;

    let unread_count_after = count_unread_inbox_entries(&store, session_id)?;
    let unread_callback_count_after = count_resumable_callback_entries(&store, session_id)?;
    let delivered_callback_count =
        unread_callback_count_before.saturating_sub(unread_callback_count_after);
    let message = if callbacks {
        format!(
            "resumed session '{session_id}' on provider '{provider_name}' with {delivered_callback_count} callback receipt(s) delivered to the next turn"
        )
    } else {
        format!("resumed session '{session_id}' on provider '{provider_name}'")
    };

    Ok(HostResumeOutcome {
        status: "completed",
        session_id,
        provider: provider_name,
        turn_id: Some(output.turn_id),
        delegated: output.delegated,
        response: output.response,
        resume_reason,
        input_message: user_message,
        unread_count_before,
        unread_count_after,
        unread_callback_count_before,
        unread_callback_count_after,
        delivered_callback_count,
        resumable: callbacks && unread_callback_count_before > 0,
        callback_pending: unread_callback_count_after > 0,
        message,
    })
}

async fn wait_for_resumable_callback_receipts(
    config: &SwitchyardConfig,
    session_id: Uuid,
    timeout_secs: u64,
    cwd: &Path,
) -> Result<HostFollowWatchOutcome, String> {
    let store = open_configured_store(config, cwd)?;
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        if let Ok(Some(session)) = store.load_session(session_id)
            && session.active_turn_is_live()
        {
            if Instant::now() >= deadline {
                let unread_count = count_unread_inbox_entries(&store, session_id).unwrap_or(0);
                return Ok(HostFollowWatchOutcome {
                    status: "wait_timeout",
                    session_id,
                    timeout_sec: timeout_secs,
                    unread_count,
                    unread_callback_count: 0,
                    items: Vec::new(),
                    message: format!(
                        "session '{session_id}' still has an active turn lease; follow stayed armed without resuming a concurrent turn"
                    ),
                });
            }

            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
            continue;
        }

        let items = load_sorted_inbox_entries(&store, session_id)?;
        let unread_count = items.iter().filter(|entry| entry.is_unread()).count();
        let resumable_items = items
            .into_iter()
            .filter(|entry| {
                entry.is_unread() && !matches!(entry.delivery_mode(), InboxDeliveryMode::Quiet)
            })
            .collect::<Vec<_>>();
        let unread_callback_count = resumable_items.len();

        if unread_callback_count > 0 {
            return Ok(HostFollowWatchOutcome {
                status: "callback_ready",
                session_id,
                timeout_sec: timeout_secs,
                unread_count,
                unread_callback_count,
                items: resumable_items,
                message: format!(
                    "resumable callback receipts are ready for session '{session_id}'; follow will continue the existing session"
                ),
            });
        }

        if Instant::now() >= deadline {
            return Ok(HostFollowWatchOutcome {
                status: "wait_timeout",
                session_id,
                timeout_sec: timeout_secs,
                unread_count,
                unread_callback_count: 0,
                items: Vec::new(),
                message: format!(
                    "no unread resumable callback receipts became ready for session '{session_id}' during this follow window; continue other work and re-arm follow later"
                ),
            });
        }

        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

fn load_sorted_inbox_entries(
    store: &StoreHandle,
    session_id: Uuid,
) -> Result<Vec<InboxEntry>, String> {
    let mut items = store
        .list_inbox_entries(session_id)
        .map_err(|err| format!("load inbox: {err}"))?;
    items.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.entry_id.cmp(&right.entry_id))
    });
    Ok(items)
}

fn apply_inbox_mutations(
    store: &mut (impl SessionInboxRepository + ?Sized),
    entries: &mut [InboxEntry],
    mark_read: bool,
    consume: bool,
) -> Result<(), String> {
    if !consume && !mark_read {
        return Ok(());
    }

    let original_entries = entries.to_vec();
    let mut mutated_any = false;

    if consume {
        for entry in entries {
            if !matches!(entry.status, InboxStatus::Consumed) {
                entry.mark_consumed();
                if let Err(err) = store.save_inbox_entry(entry) {
                    rollback_inbox_mutations(store, &original_entries)?;
                    return Err(format!("consume inbox entry: {err}"));
                }
                mutated_any = true;
            }
        }
    } else if mark_read {
        for entry in entries {
            if entry.is_unread() {
                entry.mark_read();
                if let Err(err) = store.save_inbox_entry(entry) {
                    rollback_inbox_mutations(store, &original_entries)?;
                    return Err(format!("mark inbox entry read: {err}"));
                }
                mutated_any = true;
            }
        }
    }

    if !mutated_any {
        return Ok(());
    }
    Ok(())
}

fn rollback_inbox_mutations(
    store: &mut (impl SessionInboxRepository + ?Sized),
    original_entries: &[InboxEntry],
) -> Result<(), String> {
    for entry in original_entries {
        store
            .save_inbox_entry(entry)
            .map_err(|err| format!("rollback inbox entry: {err}"))?;
    }
    Ok(())
}

fn count_unread_inbox_entries(store: &StoreHandle, session_id: Uuid) -> Result<usize, String> {
    store
        .list_inbox_entries(session_id)
        .map(|entries| {
            entries
                .into_iter()
                .filter(|entry| entry.is_unread())
                .count()
        })
        .map_err(|err| format!("reload inbox after mutation: {err}"))
}

fn count_resumable_callback_entries(
    store: &StoreHandle,
    session_id: Uuid,
) -> Result<usize, String> {
    store
        .list_inbox_entries(session_id)
        .map(|entries| {
            entries
                .into_iter()
                .filter(|entry| {
                    entry.is_unread() && !matches!(entry.delivery_mode(), InboxDeliveryMode::Quiet)
                })
                .count()
        })
        .map_err(|err| format!("reload inbox after mutation: {err}"))
}

fn resolve_host_inbox_session_id(
    store: &StoreHandle,
    session_selector: Option<&str>,
    resume_latest: bool,
) -> Result<Uuid, String> {
    match session_selector {
        Some(selector) => resolve_host_session_selector(store, selector),
        None if resume_latest => latest_host_session_id(store),
        None => latest_host_session_id(store),
    }
}

fn resolve_host_session_selector(store: &StoreHandle, selector: &str) -> Result<Uuid, String> {
    let normalized = selector.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err("`--session` cannot be empty".to_string());
    }

    let matches = store
        .list_sessions()
        .map_err(|err| format!("list sessions: {err}"))?
        .into_iter()
        .filter(|session_id| session_id.to_string().starts_with(&normalized))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [session_id] => Ok(*session_id),
        [] => Err(format!("session '{selector}' not found in selected store")),
        _ => Err(format!(
            "session prefix '{selector}' is ambiguous; matches: {}",
            matches
                .iter()
                .map(Uuid::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn latest_host_session_id(store: &StoreHandle) -> Result<Uuid, String> {
    let mut sessions = store
        .list_sessions()
        .map_err(|err| format!("list sessions: {err}"))?
        .into_iter()
        .filter_map(|session_id| {
            store
                .load_session(session_id)
                .ok()
                .flatten()
                .map(|session| (session.session_id, session.updated_at))
        })
        .collect::<Vec<_>>();

    sessions.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    sessions
        .into_iter()
        .map(|(session_id, _)| session_id)
        .next()
        .ok_or_else(|| "selected store contains no sessions".to_string())
}

fn inbox_entry_json(entry: &InboxEntry) -> serde_json::Value {
    serde_json::json!({
        "entry_id": entry.entry_id.to_string(),
        "session_id": entry.session_id.to_string(),
        "kind": entry.kind.to_string(),
        "status": entry.status.to_string(),
        "delivery_mode": entry.delivery_mode().to_string(),
        "provider": entry.provider,
        "job_id": entry.job_id.map(|id| id.to_string()),
        "turn_id": entry.turn_id.map(|id| id.to_string()),
        "title": entry.title,
        "message": entry.message,
        "summary": entry.summary,
        "payload": entry.payload,
        "created_at": entry.created_at.to_rfc3339(),
        "updated_at": entry.updated_at.to_rfc3339(),
        "read_at": entry.read_at.map(|ts| ts.to_rfc3339()),
        "consumed_at": entry.consumed_at.map(|ts| ts.to_rfc3339()),
    })
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
    let commands = host_help_json();
    println!("{}", serde_json::to_string_pretty(&commands).unwrap());
}

fn host_help_json() -> serde_json::Value {
    serde_json::json!({
        "protocol": "hyard_v2",
        "notes": [
            "delegate/await may return wait_timeout while the same job continues running in background",
            "Treat HYARD as a background tool: keep doing other useful work while long-running jobs are in flight",
            "Reuse the same job_id with status/result/await instead of starting a duplicate delegate",
            "Completed background jobs now write callback receipts into the session inbox so the next consumer can read them without polling every job id",
            "Active agents may arm `host watch` against the current session to receive callback receipts as a live wake-up/reminder channel",
            "When you already know the active session id, pass it to `host delegate --session <id>` so callback receipts are routed back to the live session instead of only the delegate worker session",
            "`host resume --callbacks` is the callback-driven continuation primitive: it noops when no unread non-quiet receipts are pending and otherwise injects them into the next routed turn",
            "`host follow` is the watch→resume primitive: by default it runs once, and `host follow --forever` keeps a resident callback consumer armed against the same session",
            "Multiple independent HYARD jobs may run in parallel when their tasks do not overlap",
            "The default delegate wait window is intentionally short; only call await immediately when your very next step is blocked on the peer result",
            "Active-job bridge JSON now includes background_recommended, await_immediately_recommended, and natural_checkpoint_recommended hints"
        ],
        "quick_start": [
            {
                "step": 1,
                "name": "delegate",
                "cli": "switchyard host delegate --provider claude --task \"Review the auth module\" --session <current-session-id> --wait-sec 1",
                "description": "Start a peer job with a short wait window so it can finish immediately or continue in background. When you know the live session id, pass --session so callback receipts return to that inbox.",
                "expected_statuses": ["completed", "wait_timeout"]
            },
            {
                "step": 2,
                "name": "keep_working",
                "cli": "switchyard host status --job-id <uuid>",
                "description": "If the first call returned wait_timeout, keep doing other useful work and reuse the same job_id to inspect progress."
            },
            {
                "step": 3,
                "name": "await_same_job",
                "cli": "switchyard host await --job-id <uuid> --timeout-sec 180",
                "description": "Wait longer later without restarting or duplicating the delegate job."
            },
            {
                "step": 4,
                "name": "read_result",
                "cli": "switchyard host result --job-id <uuid>",
                "description": "Fetch the final result once the same background job is ready."
            },
            {
                "step": 5,
                "name": "arm_callback_watch",
                "cli": "switchyard host watch --resume-latest --timeout-sec 180 --mark-read",
                "description": "Optional: wait for callback receipts from the current session so a live agent can be reminded when background work finishes."
            },
            {
                "step": 6,
                "name": "read_inbox_receipts",
                "cli": "switchyard host inbox --resume-latest --mark-read",
                "description": "Read unread callback receipts for the current/latest session so a resumed agent can pick up completed background work without remembering every job_id."
            },
            {
                "step": 7,
                "name": "follow_callbacks",
                "cli": "switchyard host follow --resume-latest --timeout-sec 180 --forever",
                "description": "Resident watch→resume consumer: keep waiting for unread non-quiet callback receipts and automatically continue the same session whenever they arrive.",
            },
            {
                "step": 8,
                "name": "resume_on_callbacks",
                "cli": "switchyard host resume --resume-latest --callbacks",
                "description": "Non-interactively continue the latest session only when unread callback receipts are ready; eligible receipts are injected into the next routed turn and then consumed."
            }
        ],
        "commands": [
            {
                "name": "/hyard:list",
                "cli": "switchyard host list",
                "description": "List available peer providers with probe status",
            },
            {
                "name": "/hyard:delegate",
                "cli": "switchyard host delegate --provider <name> --task <text> [--session <id-or-prefix>] [--wait-sec <n>]",
                "description": "Submit a peer leaf job and wait briefly; may return wait_timeout (not a failure — the same job keeps running in background). When available, pass --session so callback receipts route back to the active session inbox. Treat it as a background tool and continue with status/result/await using the same job_id.",
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
                "name": "/hyard:inbox",
                "cli": "switchyard host inbox [--session <id-or-prefix> | --resume-latest] [--all] [--mark-read | --consume]",
                "description": "Read session callback receipts produced by completed background jobs. Defaults to unread receipts for the latest session.",
            },
            {
                "name": "/hyard:watch",
                "cli": "switchyard host watch [--session <id-or-prefix> | --resume-latest] [--timeout-sec <n>] [--mark-read | --consume]",
                "description": "Wait for callback receipts from background jobs and return as soon as one arrives; useful for live reminder/wake-up flows.",
            },
            {
                "name": "/hyard:resume",
                "cli": "switchyard host resume [--session <id-or-prefix> | --resume-latest] (--callbacks | --message <text>)",
                "description": "Resume an existing session without opening the TUI. `--callbacks` only runs when unread non-quiet callback receipts are pending; otherwise it returns noop. `--message` resumes immediately with an explicit user message.",
            },
            {
                "name": "/hyard:follow",
                "cli": "switchyard host follow [--session <id-or-prefix> | --resume-latest] [--timeout-sec <n>] [--forever]",
                "description": "Wait for unread non-quiet callback receipts and automatically resume the same session when they become available. Add `--forever` to keep the callback consumer resident instead of exiting after one cycle.",
            },
            {
                "name": "/hyard:help",
                "cli": "switchyard host help",
                "description": "Print this command reference",
            },
        ],
    })
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
    obj.insert(
        "background_recommended".to_string(),
        serde_json::json!(bridge_background_recommended(
            &status,
            live_status,
            result_ready
        )),
    );
    obj.insert(
        "await_immediately_recommended".to_string(),
        serde_json::json!(bridge_await_immediately_recommended(
            command,
            &status,
            live_status,
            result_ready
        )),
    );
    obj.insert(
        "natural_checkpoint_recommended".to_string(),
        serde_json::json!(bridge_natural_checkpoint_recommended(
            command,
            &status,
            live_status,
            result_ready
        )),
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
            "{provider} job is still running in background; reuse the same job_id with status/result/await. Continue other work while the job runs."
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

fn bridge_background_recommended(
    status: &str,
    live_status: Option<HostJobStatus>,
    result_ready: bool,
) -> bool {
    match status {
        "wait_timeout" | "queued" | "running" | "cancel_requested" => true,
        "completed" if result_ready => false,
        "failed" | "cancelled" => false,
        _ => matches!(
            live_status,
            Some(HostJobStatus::Queued | HostJobStatus::Running | HostJobStatus::CancelRequested)
        ),
    }
}

fn bridge_await_immediately_recommended(
    command: &str,
    status: &str,
    live_status: Option<HostJobStatus>,
    result_ready: bool,
) -> bool {
    if result_ready {
        return false;
    }

    match status {
        "wait_timeout" | "queued" | "running" | "cancel_requested" => false,
        "failed" | "cancelled" => false,
        _ => {
            if matches!(
                live_status,
                Some(
                    HostJobStatus::Queued | HostJobStatus::Running | HostJobStatus::CancelRequested
                )
            ) {
                false
            } else {
                command == "await" && !result_ready
            }
        }
    }
}

fn bridge_natural_checkpoint_recommended(
    _command: &str,
    status: &str,
    live_status: Option<HostJobStatus>,
    result_ready: bool,
) -> bool {
    if result_ready {
        return false;
    }

    match status {
        "wait_timeout" | "queued" | "running" | "cancel_requested" => true,
        "failed" | "cancelled" => false,
        _ => matches!(
            live_status,
            Some(HostJobStatus::Queued | HostJobStatus::Running | HostJobStatus::CancelRequested)
        ),
    }
}

fn attach_timing(mut value: serde_json::Value, timing: BridgeTimingMs) -> serde_json::Value {
    let Some(obj) = value.as_object_mut() else {
        return value;
    };
    obj.insert(
        "timing_ms".to_string(),
        serde_json::json!({
            "launch": timing.launch_ms,
            "wait": timing.wait_ms,
            "total": timing.total_ms,
        }),
    );
    value
}

fn clamp_millis_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
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
    std::fs::File::create(&stdout_log_path).map_err(|err| {
        format!(
            "create worker stdout log '{}': {err}",
            stdout_log_path.display()
        )
    })?;
    std::fs::File::create(&stderr_log_path).map_err(|err| {
        format!(
            "create worker stderr log '{}': {err}",
            stderr_log_path.display()
        )
    })?;
    let mut worker = spawn_worker_process(&exe, cwd, job_id, &stdout_log_path, &stderr_log_path)?;
    let child_pid = worker.id();

    job_store
        .update(job_id, |job| {
            if !job.status.is_terminal() {
                job.pid = Some(child_pid);
                job.last_event = Some("worker_spawned".to_string());
            }
        })
        .map_err(|err| format!("record spawned worker pid: {err}"))?;

    wait_for_worker_boot(job_store, job_id, &mut worker)?;
    Ok(())
}

fn spawn_worker_process(
    exe: &Path,
    cwd: &Path,
    job_id: Uuid,
    stdout_log_path: &Path,
    stderr_log_path: &Path,
) -> Result<SpawnedWorker, String> {
    #[cfg(windows)]
    {
        match spawn_worker_process_with_handle_list(
            exe,
            cwd,
            job_id,
            stdout_log_path,
            stderr_log_path,
            true,
        ) {
            Ok(worker) => Ok(worker),
            Err(primary_err) => match spawn_worker_process_with_handle_list(
                exe,
                cwd,
                job_id,
                stdout_log_path,
                stderr_log_path,
                false,
            ) {
                Ok(worker) => Ok(worker),
                Err(fallback_handle_list_err) => {
                    match spawn_worker_process_once(
                        exe,
                        cwd,
                        job_id,
                        stdout_log_path,
                        stderr_log_path,
                        true,
                    ) {
                        Ok(child) => Ok(child),
                        Err(fallback_primary_err) => {
                            match spawn_worker_process_once(
                                exe,
                                cwd,
                                job_id,
                                stdout_log_path,
                                stderr_log_path,
                                false,
                            ) {
                                Ok(child) => Ok(child),
                                Err(fallback_err) => Err(format!(
                                    "spawn host worker with explicit handle list: {primary_err}; without breakaway: {fallback_handle_list_err}; direct spawn fallback: {fallback_primary_err}; secondary fallback failed: {fallback_err}"
                                )),
                            }
                        }
                    }
                }
            },
        }
    }

    #[cfg(not(windows))]
    {
        spawn_worker_process_once(exe, cwd, job_id, stdout_log_path, stderr_log_path, false)
    }
}

enum SpawnedWorker {
    Attached(std::process::Child),
    DetachedPid(u32),
}

impl SpawnedWorker {
    fn id(&self) -> u32 {
        match self {
            Self::Attached(child) => child.id(),
            Self::DetachedPid(pid) => *pid,
        }
    }
}

fn spawn_worker_process_once(
    exe: &Path,
    cwd: &Path,
    job_id: Uuid,
    stdout_log_path: &Path,
    stderr_log_path: &Path,
    #[allow(unused_variables)] try_breakaway: bool,
) -> Result<SpawnedWorker, String> {
    let stdout_log = open_worker_log_append(stdout_log_path)?;
    let stderr_log = open_worker_log_append(stderr_log_path)?;

    let mut command = std::process::Command::new(exe);
    command
        .args(["host", "worker", "--job-id", &job_id.to_string()])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log));

    #[cfg(windows)]
    apply_windows_worker_creation_flags(&mut command, try_breakaway);

    command
        .spawn()
        .map(SpawnedWorker::Attached)
        .map_err(|err| format!("spawn host worker: {err}"))
}

fn open_worker_log_append(path: &Path) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| format!("open worker log '{}' for append: {err}", path.display()))
}

#[cfg(windows)]
fn apply_windows_worker_creation_flags(command: &mut std::process::Command, try_breakaway: bool) {
    let mut flags = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    if try_breakaway {
        flags |= CREATE_BREAKAWAY_FROM_JOB;
    }
    command.creation_flags(flags);
}

#[cfg(windows)]
fn spawn_worker_process_with_handle_list(
    exe: &Path,
    cwd: &Path,
    job_id: Uuid,
    stdout_log_path: &Path,
    stderr_log_path: &Path,
    try_breakaway: bool,
) -> Result<SpawnedWorker, String> {
    let stdin_null = std::fs::OpenOptions::new()
        .read(true)
        .open("NUL")
        .map_err(|err| format!("open NUL for worker stdin: {err}"))?;
    let stdout_log = open_worker_log_append(stdout_log_path)?;
    let stderr_log = open_worker_log_append(stderr_log_path)?;

    let stdin_handle = duplicate_inheritable_handle(&stdin_null, "stdin")?;
    let stdout_handle = duplicate_inheritable_handle(&stdout_log, "stdout")?;
    let stderr_handle = duplicate_inheritable_handle(&stderr_log, "stderr")?;
    let stdio_handles = [
        stdin_handle.as_raw_handle() as HANDLE,
        stdout_handle.as_raw_handle() as HANDLE,
        stderr_handle.as_raw_handle() as HANDLE,
    ];

    let mut attributes = ProcThreadAttributeList::with_capacity(1)?;
    attributes.set_handle_list(&stdio_handles)?;

    let mut startup_info = STARTUPINFOEXW::default();
    startup_info.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup_info.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup_info.StartupInfo.hStdInput = stdio_handles[0];
    startup_info.StartupInfo.hStdOutput = stdio_handles[1];
    startup_info.StartupInfo.hStdError = stdio_handles[2];
    startup_info.lpAttributeList = attributes.as_mut_ptr();

    let application_name = wide_null(exe.as_os_str());
    let current_dir = wide_null(cwd.as_os_str());
    let mut command_line = build_worker_command_line(exe, job_id);
    let mut process_info = PROCESS_INFORMATION::default();

    let mut creation_flags =
        EXTENDED_STARTUPINFO_PRESENT | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    if try_breakaway {
        creation_flags |= CREATE_BREAKAWAY_FROM_JOB;
    }

    let created = unsafe {
        CreateProcessW(
            application_name.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            creation_flags,
            ptr::null(),
            current_dir.as_ptr(),
            &startup_info.StartupInfo,
            &mut process_info,
        )
    };

    if created == 0 {
        return Err(format!(
            "spawn host worker via CreateProcessW: {}",
            std::io::Error::last_os_error()
        ));
    }

    let pid = process_info.dwProcessId;
    let _worker_thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread as RawHandle) };
    let _worker_process =
        unsafe { OwnedHandle::from_raw_handle(process_info.hProcess as RawHandle) };

    Ok(SpawnedWorker::DetachedPid(pid))
}

#[cfg(windows)]
fn duplicate_inheritable_handle(file: &std::fs::File, label: &str) -> Result<OwnedHandle, String> {
    let mut duplicated: HANDLE = ptr::null_mut();
    let current_process = unsafe { GetCurrentProcess() };
    let duplicated_ok = unsafe {
        DuplicateHandle(
            current_process,
            file.as_raw_handle() as HANDLE,
            current_process,
            &mut duplicated,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    };

    if duplicated_ok == 0 {
        return Err(format!(
            "duplicate worker {label} handle for inheritance: {}",
            std::io::Error::last_os_error()
        ));
    }

    Ok(unsafe { OwnedHandle::from_raw_handle(duplicated as RawHandle) })
}

#[cfg(windows)]
fn build_worker_command_line(exe: &Path, job_id: Uuid) -> Vec<u16> {
    let mut command_line = OsString::from("\"");
    command_line.push(exe.as_os_str());
    command_line.push("\" host worker --job-id ");
    command_line.push(job_id.to_string());
    wide_null(command_line.as_os_str())
}

#[cfg(windows)]
fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

#[cfg(windows)]
struct ProcThreadAttributeList {
    data: Vec<u8>,
}

#[cfg(windows)]
impl ProcThreadAttributeList {
    fn with_capacity(attribute_count: u32) -> Result<Self, String> {
        let mut bytes_required = 0usize;
        unsafe {
            InitializeProcThreadAttributeList(
                ptr::null_mut(),
                attribute_count,
                0,
                &mut bytes_required,
            )
        };

        let mut data = vec![0u8; bytes_required];
        let initialized = unsafe {
            InitializeProcThreadAttributeList(
                data.as_mut_ptr() as *mut _,
                attribute_count,
                0,
                &mut bytes_required,
            )
        };
        if initialized == 0 {
            return Err(format!(
                "InitializeProcThreadAttributeList: {}",
                std::io::Error::last_os_error()
            ));
        }

        Ok(Self { data })
    }

    fn as_mut_ptr(&mut self) -> *mut core::ffi::c_void {
        self.data.as_mut_ptr() as *mut _
    }

    fn set_handle_list(&mut self, handles: &[HANDLE]) -> Result<(), String> {
        let updated = unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                ptr::null_mut(),
                ptr::null(),
            )
        };

        if updated == 0 {
            return Err(format!(
                "UpdateProcThreadAttribute(PROC_THREAD_ATTRIBUTE_HANDLE_LIST): {}",
                std::io::Error::last_os_error()
            ));
        }

        Ok(())
    }
}

#[cfg(windows)]
impl Drop for ProcThreadAttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

fn wait_for_worker_boot(
    job_store: &HostJobStore,
    job_id: Uuid,
    worker: &mut SpawnedWorker,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_millis(WORKER_BOOT_TIMEOUT_MS);

    loop {
        match worker {
            SpawnedWorker::Attached(child) => {
                if let Some(status) = child
                    .try_wait()
                    .map_err(|err| format!("check host worker status: {err}"))?
                {
                    let job = job_store
                        .load(job_id)
                        .map_err(|err| format!("load worker manifest '{job_id}': {err}"))?
                        .ok_or_else(|| {
                            format!("job '{job_id}' disappeared during worker startup")
                        })?;

                    if is_stalled_at_worker_boot(&job) {
                        return Err(build_worker_boot_failure_message(job_store, job_id, status));
                    }

                    return Ok(());
                }
            }
            SpawnedWorker::DetachedPid(pid) => {
                if !switchyard_host_jobs::is_process_alive(*pid) {
                    let job = job_store
                        .load(job_id)
                        .map_err(|err| format!("load worker manifest '{job_id}': {err}"))?
                        .ok_or_else(|| {
                            format!("job '{job_id}' disappeared during worker startup")
                        })?;

                    if is_stalled_at_worker_boot(&job) {
                        return Err(build_worker_detached_boot_failure_message(
                            job_store, job_id, *pid, job.status,
                        ));
                    }

                    return Ok(());
                }
            }
        };

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

fn build_worker_detached_boot_failure_message(
    job_store: &HostJobStore,
    job_id: Uuid,
    pid: u32,
    status: HostJobStatus,
) -> String {
    let mut message = format!("host worker pid {pid} disappeared while job remained {status}");

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
    mut timing: BridgeTimingMs,
    total_start: Instant,
) {
    let wait_start = Instant::now();
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
    timing.wait_ms = clamp_millis_u64(wait_start.elapsed());
    timing.total_ms = clamp_millis_u64(total_start.elapsed());

    if job.status.is_terminal() {
        emit_json(&attach_timing(job_bridge_json(command, &job), timing));
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
    emit_json(&attach_timing(
        wait_timeout_bridge_json(command, &timed_out),
        timing,
    ));
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
    let full_message = build_host_job_message(
        task,
        (!peer_catalog.peers.is_empty()).then_some(hyard_guidance.as_str()),
    );

    Ok(PreparedHostJobRun {
        provider_name: provider_name.to_string(),
        provider_impl,
        full_message,
        config: config.clone(),
        cwd: cwd.to_path_buf(),
        artifact_dir: config.artifact_dir(cwd),
    })
}

fn build_host_job_message(task: &str, hyard_guidance: Option<&str>) -> String {
    let mut sections = vec![task.trim().to_string()];
    if let Some(guidance) = hyard_guidance
        && !guidance.trim().is_empty()
    {
        sections.push(guidance.trim().to_string());
    }
    sections.push(peer_result_contract().to_string());
    sections.join("\n\n---\n")
}

fn peer_result_contract() -> &'static str {
    concat!(
        "Peer task result contract:\n",
        "- You are acting as a leaf peer for another model.\n",
        "- Return only the requested deliverable or findings.\n",
        "- Do not narrate your process with lines like \"I will...\", \"I am going to...\", or \"I analyzed...\".\n",
        "- Prefer concise bullets, tables, or short sections when helpful.\n",
        "- If the task asks for an exact output shape, follow it exactly.\n",
        "- Do not delegate further."
    )
}

fn compact_host_job_summary(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(stripped) = strip_inline_narration_prefix_to_structured_marker(trimmed) {
        return Some(stripped);
    }

    let lines: Vec<&str> = trimmed.lines().collect();
    let start = lines
        .iter()
        .position(|line| looks_like_structured_result_line(line.trim()))
        .filter(|idx| {
            lines[..*idx]
                .iter()
                .filter_map(|line| {
                    let trimmed = line.trim();
                    (!trimmed.is_empty()).then_some(trimmed)
                })
                .all(is_process_narration_line)
        })
        .unwrap_or(0);

    let compact = lines[start..].join("\n");
    Some(compact.trim().to_string()).filter(|value| !value.is_empty())
}

fn compact_live_job_preview(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lines: Vec<&str> = trimmed.lines().collect();
    let start = lines
        .iter()
        .position(|line| !is_runtime_noise_preview_line(line.trim()))
        .unwrap_or(lines.len());
    let compact = lines[start..].join("\n");
    compact_host_job_summary(&compact)
}

fn strip_inline_narration_prefix_to_structured_marker(text: &str) -> Option<String> {
    let mut candidates = Vec::new();
    for needle in ["* ", "- ", "1. ", "2. ", "3. "] {
        if let Some(idx) = text.find(needle)
            && idx > 0
        {
            candidates.push(idx);
        }
    }
    let first = candidates.into_iter().min()?;
    let prefix = text[..first].trim().trim_end_matches(['.', ':', ';', ',']);
    if is_process_narration_line(prefix) {
        return Some(text[first..].trim().to_string());
    }
    None
}

fn looks_like_structured_result_line(line: &str) -> bool {
    if line.is_empty() {
        return false;
    }

    line.starts_with("- ")
        || line.starts_with("* ")
        || line.starts_with("##")
        || line.starts_with('#')
        || line.starts_with("| ")
        || line.starts_with("|")
        || line.chars().next().is_some_and(|ch| ch.is_ascii_digit()) && line.contains(". ")
}

fn is_runtime_noise_preview_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }

    let lower = trimmed.to_ascii_lowercase();
    lower.contains("[执行]")
        || lower.contains("命令已解析")
        || lower.contains("npm wrapper 已改写")
        || lower.contains("使用命令")
        || lower.contains("resolved command")
        || lower.contains("npm wrapper rewritten")
}

fn is_process_narration_line(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    [
        "i will ",
        "i'll ",
        "i am ",
        "i'm ",
        "i have ",
        "i've ",
        "i began ",
        "i analyzed ",
        "i will now ",
        "let me ",
        "now i ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn apply_live_job_preview(job: &mut HostJobState, preview: Option<&str>) {
    let Some(preview) = preview else {
        return;
    };
    if let Some(compact) = compact_live_job_preview(preview) {
        job.set_last_preview(Some(&compact));
    }
}

fn build_job_callback_entry(job: &HostJobState) -> Option<InboxEntry> {
    let session_id = job.callback_session_id.or(job.session_id)?;
    let title = match job.status {
        HostJobStatus::Completed => format!("{} background job completed", job.provider),
        HostJobStatus::Cancelled => format!("{} background job cancelled", job.provider),
        HostJobStatus::Failed => format!("{} background job failed", job.provider),
        HostJobStatus::Queued | HostJobStatus::Running | HostJobStatus::CancelRequested => {
            format!("{} background job updated", job.provider)
        }
    };
    let message = match job.status {
        HostJobStatus::Completed => {
            format!(
                "{} finished a background task while you were idle.",
                job.provider
            )
        }
        HostJobStatus::Cancelled => {
            format!(
                "{} background task was cancelled before completion.",
                job.provider
            )
        }
        HostJobStatus::Failed => {
            format!("{} background task failed and needs review.", job.provider)
        }
        HostJobStatus::Queued | HostJobStatus::Running | HostJobStatus::CancelRequested => {
            format!("{} background task changed state.", job.provider)
        }
    };
    let mut entry = InboxEntry::background_job_receipt(session_id, &job.provider, title, message);
    entry.job_id = Some(job.job_id);
    entry.turn_id = job.turn_id;
    entry.summary = job
        .result_summary
        .clone()
        .or_else(|| job.last_output_preview.clone())
        .or_else(|| job.error.clone());
    entry.payload = serde_json::json!({
        "job_id": job.job_id.to_string(),
        "provider": job.provider.clone(),
        "job_status": job.status.to_string(),
        "callback_session_id": job.callback_session_id.map(|id| id.to_string()),
        "worker_session_id": job.session_id.map(|id| id.to_string()),
        "turn_id": job.turn_id.map(|id| id.to_string()),
        "artifact_count": job.artifact_count,
        "result_ready": job.result_ready,
        "wait_timeout_count": job.wait_timeout_count,
        "last_event": job.last_event.clone(),
        "summary": job.result_summary.clone(),
        "error": job.error.clone(),
    });
    Some(entry)
}

fn emit_job_callback_receipt_if_needed(
    store: &mut StoreHandle,
    job_store: &HostJobStore,
    job: &HostJobState,
) -> Result<(), String> {
    if !job.status.is_terminal() || job.callback_emitted_at.is_some() {
        return Ok(());
    }

    let Some(entry) = build_job_callback_entry(job) else {
        if job.session_id.is_none() {
            eprintln!(
                "[hyard] WARNING: job '{}' reached terminal state '{}' without session_id; callback receipt dropped",
                job.job_id, job.status
            );
        }
        return Ok(());
    };

    store.save_inbox_entry(&entry).map_err(|err| {
        format!(
            "save callback receipt for session '{}': {err}",
            entry.session_id
        )
    })?;

    job_store
        .update(job.job_id, |persisted| {
            persisted.callback_inbox_id = Some(entry.entry_id);
            persisted.callback_emitted_at = Some(Utc::now());
        })
        .map_err(|err| {
            format!(
                "mark callback receipt emitted for job '{}': {err}",
                job.job_id
            )
        })?;

    Ok(())
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
        prepared.config.store_backend(&prepared.cwd),
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

    let policy = execution_policy_from_config(&prepared.config, &prepared.cwd);
    let run_result = run_turn_phased_with_policy(
        &mut store,
        &mut session,
        prepared.provider_impl.as_ref(),
        prepared.full_message,
        prepared.cwd.clone(),
        Some(&prepared.artifact_dir),
        Some(&runtime_tx),
        TurnPhase::Normal,
        cancel.clone(),
        policy,
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
            let compact_summary = final_turn
                .provider_response
                .as_deref()
                .and_then(compact_host_job_summary);

            let final_job = job_store
                .update(job_id, |job| {
                    let preview_text = compact_summary
                        .clone()
                        .or_else(|| final_turn.provider_response.clone())
                        .or_else(|| job.last_output_preview.clone());
                    job.turn_id = Some(output.turn_id);
                    job.completed_at = Some(Utc::now());
                    job.result_summary = compact_summary
                        .clone()
                        .or_else(|| final_turn.provider_response.clone());
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
            emit_job_callback_receipt_if_needed(&mut store, &job_store, &final_job)?;
            Ok(())
        }
        Err(err) => {
            let final_job = job_store
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
            emit_job_callback_receipt_if_needed(&mut store, &job_store, &final_job)?;
            Ok(())
        }
    }
}

fn apply_runtime_event(job: &mut HostJobState, event: &RuntimeEvent) {
    match event {
        RuntimeEvent::TurnPreparing {
            provider, phase, ..
        } => {
            job.status = HostJobStatus::Running;
            job.last_event = Some(format!("turn_preparing:{provider}:{phase}"));
        }
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
            ..
        } => {
            job.status = HostJobStatus::Running;
            job.turn_id = Some(*turn_id);
            job.last_event = Some(format!("item_updated:{provider}"));
            apply_live_job_preview(job, Some(text));
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
            apply_live_job_preview(job, Some(text));
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
            apply_live_job_preview(job, response.as_deref());
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
        RuntimeEvent::CallbackReceiptsInjected { .. }
        | RuntimeEvent::DelegateRequested { .. }
        | RuntimeEvent::HyardJobObserved { .. }
        | RuntimeEvent::PeerExecutionTelemetry { .. }
        | RuntimeEvent::PeerTurnStarted { .. }
        | RuntimeEvent::PeerItemUpdated { .. }
        | RuntimeEvent::PeerTerminalOutput { .. }
        | RuntimeEvent::DelegateCompleted { .. }
        | RuntimeEvent::PeerOutputCompleted { .. }
        | RuntimeEvent::FinalizationStarted { .. }
        | RuntimeEvent::WorkerSpawned { .. }
        | RuntimeEvent::WorkerStateChanged { .. }
        | RuntimeEvent::WorkerRetrying { .. }
        | RuntimeEvent::WorkerTerminated { .. } => {}
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
    use switchyard_store::StoreError;

    #[derive(Default)]
    struct FailingInboxStore {
        entries: Vec<InboxEntry>,
        fail_on_save_number: Option<usize>,
        save_calls: usize,
    }

    impl SessionInboxRepository for FailingInboxStore {
        fn save_inbox_entry(&mut self, entry: &InboxEntry) -> Result<(), StoreError> {
            self.save_calls += 1;
            if self.fail_on_save_number == Some(self.save_calls) {
                return Err(StoreError::Io(std::io::Error::other(
                    "injected save failure",
                )));
            }

            if let Some(existing) = self
                .entries
                .iter_mut()
                .find(|existing| existing.entry_id == entry.entry_id)
            {
                *existing = entry.clone();
            } else {
                self.entries.push(entry.clone());
            }
            Ok(())
        }

        fn list_inbox_entries(&self, _session_id: Uuid) -> Result<Vec<InboxEntry>, StoreError> {
            Ok(self.entries.clone())
        }
    }

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

        let json = attach_timing(
            wait_timeout_bridge_json("delegate", &job),
            BridgeTimingMs {
                launch_ms: 1,
                wait_ms: 2,
                total_ms: 3,
            },
        );

        assert_eq!(json["protocol"], HYARD_PROTOCOL);
        assert_eq!(json["command"], "delegate");
        assert_eq!(json["status"], "wait_timeout");
        assert!(
            json["message"]
                .as_str()
                .is_some_and(|message| message.contains("same job_id"))
        );
        assert!(
            json["message"]
                .as_str()
                .is_some_and(|message| message.contains("other work"))
        );
        assert_eq!(json["timing_ms"]["launch"], 1);
        assert_eq!(json["timing_ms"]["wait"], 2);
        assert_eq!(json["timing_ms"]["total"], 3);
        let next_actions = json["next_actions"].as_array().cloned().unwrap_or_default();
        assert!(next_actions.contains(&serde_json::json!("status")));
        assert!(next_actions.contains(&serde_json::json!("result")));
        assert!(next_actions.contains(&serde_json::json!("await")));
        assert_eq!(json["background_recommended"], true);
        assert_eq!(json["await_immediately_recommended"], false);
        assert_eq!(json["natural_checkpoint_recommended"], true);
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
        assert_eq!(json["background_recommended"], true);
        assert_eq!(json["await_immediately_recommended"], false);
        assert_eq!(json["natural_checkpoint_recommended"], true);
    }

    #[test]
    fn host_job_message_appends_peer_result_contract() {
        let message = build_host_job_message("Review auth module", Some("HYARD guidance here"));
        assert!(message.contains("Review auth module"));
        assert!(message.contains("HYARD guidance here"));
        assert!(message.contains("Peer task result contract"));
        assert!(message.contains("Do not narrate your process"));
    }

    #[test]
    fn compact_host_job_summary_drops_process_narration_before_structured_results() {
        let raw = "\
I will begin by inspecting the current implementation.\n\
I have analyzed the current command surface.\n\
\n\
1. Add --tail to store show\n\
2. Add --sort to list-sessions\n";

        let compact = compact_host_job_summary(raw).expect("compact summary");
        assert!(compact.starts_with("1. Add --tail to store show"));
        assert!(!compact.contains("I will begin"));
    }

    #[test]
    fn compact_host_job_summary_keeps_plain_findings_when_no_structured_block_exists() {
        let raw = "Need one more index on the sessions table for updated_at lookups.";
        let compact = compact_host_job_summary(raw).expect("compact summary");
        assert_eq!(compact, raw);
    }

    #[test]
    fn compact_host_job_summary_strips_inline_narration_before_bullet() {
        let raw =
            "I will inspect the store show flow first. * Add --tail <N> to limit long output.";
        let compact = compact_host_job_summary(raw).expect("compact summary");
        assert_eq!(compact, "* Add --tail <N> to limit long output.");
    }

    #[test]
    fn host_help_json_mentions_background_usage_and_parallelism() {
        let json = host_help_json();
        let notes = json["notes"].as_array().cloned().unwrap_or_default();
        let rendered = serde_json::to_string(&notes).unwrap();
        assert!(rendered.contains("background tool"));
        assert!(rendered.contains("other useful work"));
        assert!(rendered.contains("parallel"));
        assert!(rendered.contains("background_recommended"));
        assert!(rendered.contains("host watch"));
        assert!(rendered.contains("host resume"));
        assert!(rendered.contains("host follow"));
        assert!(rendered.contains("--forever"));
    }

    #[test]
    fn host_help_json_includes_quick_start_background_flow() {
        let json = host_help_json();
        let quick_start = json["quick_start"].as_array().cloned().unwrap_or_default();
        let rendered = serde_json::to_string(&quick_start).unwrap();
        assert!(rendered.contains("delegate"));
        assert!(rendered.contains("wait_timeout"));
        assert!(rendered.contains("status"));
        assert!(rendered.contains("await"));
        assert!(rendered.contains("result"));
        assert!(rendered.contains("inbox"));
        assert!(rendered.contains("watch"));
        assert!(rendered.contains("resume"));
        assert!(rendered.contains("follow"));
        assert!(rendered.contains("--forever"));
        assert!(rendered.contains("same job_id") || rendered.contains("same background job"));
    }

    #[test]
    fn build_job_callback_entry_captures_summary_and_identifiers() {
        let session_id = Uuid::now_v7();
        let turn_id = Uuid::now_v7();
        let mut job = HostJobState::new("gemini", "task", PathBuf::from("."));
        job.session_id = Some(session_id);
        job.turn_id = Some(turn_id);
        job.status = HostJobStatus::Completed;
        job.result_ready = true;
        job.result_summary = Some("UI mock looks good".to_string());
        job.artifact_count = 2;

        let entry = build_job_callback_entry(&job).expect("callback entry");
        assert_eq!(entry.session_id, session_id);
        assert_eq!(entry.job_id, Some(job.job_id));
        assert_eq!(entry.turn_id, Some(turn_id));
        assert_eq!(
            entry.kind,
            switchyard_session::InboxItemKind::BackgroundJobReceipt
        );
        assert_eq!(entry.status, InboxStatus::Unread);
        assert!(entry.title.contains("completed"));
        assert_eq!(entry.summary.as_deref(), Some("UI mock looks good"));
        assert_eq!(entry.payload["artifact_count"], 2);
    }

    #[test]
    fn inbox_entry_json_includes_delivery_mode() {
        let session_id = Uuid::now_v7();
        let mut entry = InboxEntry::background_job_receipt(session_id, "claude", "done", "message");
        entry.payload = serde_json::json!({ "job_status": "failed" });

        let json = inbox_entry_json(&entry);
        assert_eq!(json["delivery_mode"], "immediate");
    }

    #[test]
    fn compact_live_job_preview_drops_execution_telemetry_only_output() {
        assert_eq!(
            compact_live_job_preview("[执行] 命令已解析：claude -> claude.exe"),
            None
        );
        assert_eq!(
            compact_live_job_preview("[STDIO] [执行] npm wrapper 已改写：gemini.cmd -> gemini.js"),
            None
        );
    }

    #[test]
    fn compact_live_job_preview_strips_execution_telemetry_prefix() {
        let preview = compact_live_job_preview(
            "[执行] 命令已解析：claude -> claude.exe\n\n* Add a quick-start example for wait_timeout recovery.",
        )
        .expect("preview");
        assert_eq!(
            preview,
            "* Add a quick-start example for wait_timeout recovery."
        );
    }

    #[test]
    fn apply_runtime_event_keeps_existing_preview_when_new_update_is_only_execution_noise() {
        let turn_id = Uuid::now_v7();
        let mut job = HostJobState::new("claude", "task", PathBuf::from("."));
        job.set_last_preview(Some("Meaningful progress update"));

        apply_runtime_event(
            &mut job,
            &RuntimeEvent::CoreItemUpdated {
                turn_id,
                provider: "claude".to_string(),
                event_type: "item_updated".to_string(),
                text: "[执行] 命令已解析：claude -> claude.exe".to_string(),
                payload: None,
            },
        );

        assert_eq!(
            job.last_output_preview.as_deref(),
            Some("Meaningful progress update")
        );
    }

    #[test]
    fn attach_timing_inserts_launch_wait_and_total_millis() {
        let value = serde_json::json!({"status": "wait_timeout"});
        let timed = attach_timing(
            value,
            BridgeTimingMs {
                launch_ms: 12,
                wait_ms: 34,
                total_ms: 56,
            },
        );
        assert_eq!(timed["timing_ms"]["launch"], 12);
        assert_eq!(timed["timing_ms"]["wait"], 34);
        assert_eq!(timed["timing_ms"]["total"], 56);
    }

    #[test]
    fn apply_inbox_mutations_rolls_back_partial_mark_read_failures() {
        let session_id = Uuid::now_v7();
        let first = InboxEntry::background_job_receipt(session_id, "claude", "done-1", "message-1");
        let second =
            InboxEntry::background_job_receipt(session_id, "gemini", "done-2", "message-2");
        let mut store = FailingInboxStore {
            entries: vec![first.clone(), second.clone()],
            fail_on_save_number: Some(2),
            save_calls: 0,
        };
        let mut returned = vec![first.clone(), second.clone()];

        let error = apply_inbox_mutations(&mut store, &mut returned, true, false)
            .expect_err("second save should fail and trigger rollback");
        assert!(error.contains("mark inbox entry read"));

        let persisted = store.list_inbox_entries(session_id).unwrap();
        assert_eq!(persisted.len(), 2);
        assert!(persisted.iter().all(InboxEntry::is_unread));
        assert_eq!(store.save_calls, 4, "2 writes + 2 rollback writes");
    }

    #[test]
    fn apply_inbox_mutations_rolls_back_partial_consume_failures() {
        let session_id = Uuid::now_v7();
        let first = InboxEntry::background_job_receipt(session_id, "claude", "done-1", "message-1");
        let second =
            InboxEntry::background_job_receipt(session_id, "gemini", "done-2", "message-2");
        let mut store = FailingInboxStore {
            entries: vec![first.clone(), second.clone()],
            fail_on_save_number: Some(2),
            save_calls: 0,
        };
        let mut returned = vec![first.clone(), second.clone()];

        let error = apply_inbox_mutations(&mut store, &mut returned, false, true)
            .expect_err("second consume save should fail and trigger rollback");
        assert!(error.contains("consume inbox entry"));

        let persisted = store.list_inbox_entries(session_id).unwrap();
        assert_eq!(persisted.len(), 2);
        assert!(persisted.iter().all(InboxEntry::is_unread));
        assert_eq!(store.save_calls, 4, "2 writes + 2 rollback writes");
    }
}
