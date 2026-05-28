//! Routed turn: single user message that may trigger a core -> peer -> core loop.
//!
//! V1 constraints:
//! - Max 3 router iterations (core, delegate, core finalization)
//! - Only 1 delegate task per request
//! - Peers cannot delegate further

use std::path::{Path, PathBuf};

use switchyard_host_jobs::list_job_summaries;
use switchyard_text::{preview_chars, preview_collapsed};
use uuid::Uuid;

use switchyard_config::SwitchyardConfig;
use switchyard_provider_api::{
    DelegateRequest, ExecutionPolicy, InputAttachment, LiveInstanceRegistry, PeerCatalog, Provider,
    TurnInput, extract_sentinel_blocks, render_delegate_result_block, strip_sentinel_blocks,
};
use switchyard_session::{InboxDeliveryMode, InboxEntry, Session, TurnStatus};
use switchyard_store::CanonicalStore;

use crate::error::CoreError;
use crate::turn_runner::TurnOutput;

const MAX_ROUTER_LOOPS: usize = 3;
const MAX_CONTINUATION_HINT_JOBS: usize = 3;
const ROUTER_RUNTIME_OBSERVER_BUFFER: usize = 1024;

/// Controls whether Switchyard injects orchestration instructions into the
/// provider-facing prompt for this routed turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterPromptInjection {
    /// Legacy agent/orchestration mode: include peer delegation instructions,
    /// HYARD live-session hints, continuation hints, and callback receipts in
    /// the prompt so the model can drive delegation itself.
    VerboseOrchestration,
    /// Clean chat mode: send only the user's authored text plus structured
    /// attachments. No Switchyard debug/delegation/HYARD boilerplate is added
    /// to the model prompt.
    Clean,
}

fn spawn_runtime_event_forwarder(
    tx: &tokio::sync::mpsc::Sender<crate::runtime_events::RuntimeEvent>,
) -> tokio::sync::mpsc::Sender<crate::runtime_events::RuntimeEvent> {
    let (live_tx, mut live_rx) = tokio::sync::mpsc::channel(ROUTER_RUNTIME_OBSERVER_BUFFER);
    let tx = tx.clone();
    tokio::spawn(async move {
        while let Some(evt) = live_rx.recv().await {
            if tx.send(evt).await.is_err() {
                break;
            }
        }
    });
    live_tx
}

fn try_forward_observed_runtime_event(
    live_tx: &tokio::sync::mpsc::Sender<crate::runtime_events::RuntimeEvent>,
    event: crate::runtime_events::RuntimeEvent,
) {
    match live_tx.try_send(event) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
            if runtime_observer_event_should_preserve_on_overflow(&event) {
                let live_tx = live_tx.clone();
                tokio::spawn(async move {
                    let _ = live_tx.send(event).await;
                });
            }
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
    }
}

fn runtime_observer_event_should_preserve_on_overflow(
    event: &crate::runtime_events::RuntimeEvent,
) -> bool {
    match event {
        crate::runtime_events::RuntimeEvent::PeerTerminalOutput { .. } => false,
        crate::runtime_events::RuntimeEvent::PeerItemUpdated { payload, .. } => {
            runtime_observer_payload_is_control_event(payload.as_ref())
        }
        _ => true,
    }
}

fn runtime_observer_payload_is_control_event(payload: Option<&serde_json::Value>) -> bool {
    fn has_control_token(text: &str) -> bool {
        let text = text.to_ascii_lowercase();
        text.contains("approval")
            || text.contains("permission")
            || text.contains("confirm")
            || text.contains("server_request")
    }

    fn scan(value: &serde_json::Value, depth: usize, visited: &mut usize) -> bool {
        if depth > 8 || *visited > 256 {
            return false;
        }
        *visited += 1;
        match value {
            serde_json::Value::String(text) => has_control_token(text),
            serde_json::Value::Array(items) => {
                items.iter().any(|item| scan(item, depth + 1, visited))
            }
            serde_json::Value::Object(map) => map
                .iter()
                .any(|(key, value)| has_control_token(key) || scan(value, depth + 1, visited)),
            _ => false,
        }
    }

    let Some(payload) = payload else {
        return false;
    };
    let mut visited = 0;
    scan(payload, 0, &mut visited)
}

impl RouterPromptInjection {
    fn includes_orchestration(self) -> bool {
        matches!(self, Self::VerboseOrchestration)
    }
}

/// Result of a routed turn, which may include delegation.
pub struct RoutedTurnOutput {
    /// The final turn_id (the last core turn).
    pub turn_id: Uuid,
    /// The final response text to show the user.
    pub response: Option<String>,
    /// Whether delegation occurred during this routed turn.
    pub delegated: bool,
}

fn build_initial_router_message(
    user_message: &str,
    peer_catalog: &PeerCatalog,
    continuation_hint: Option<&str>,
) -> String {
    let mut sections = vec![user_message.to_string()];

    if !peer_catalog.peers.is_empty() {
        sections.push(peer_catalog.render_prompt_block());
    }

    if let Some(hint) = continuation_hint.filter(|hint| !hint.trim().is_empty()) {
        sections.push(hint.to_string());
    }

    sections.join("\n\n---\n")
}

fn build_first_iteration_router_message(
    user_message: &str,
    peer_catalog: &PeerCatalog,
    continuation_hint: Option<&str>,
    prompt_injection: RouterPromptInjection,
) -> String {
    if prompt_injection.includes_orchestration() {
        build_initial_router_message(user_message, peer_catalog, continuation_hint)
    } else {
        user_message.to_string()
    }
}

fn build_finalization_router_message(
    user_message: &str,
    delegate_context: &str,
    continuation_hint: Option<&str>,
) -> String {
    let mut message = format!(
        "Original user task:\n{user_message}\n\n\
         A delegate task has been completed. Here is the result:\n\n\
         {delegate_context}"
    );

    if let Some(hint) = continuation_hint.filter(|hint| !hint.trim().is_empty()) {
        message.push_str("\n\n");
        message.push_str(hint);
    }

    message.push_str(
        "\n\nIncorporate the delegate's findings into your response. \
         Provide the final answer to the user's original task. \
         Do NOT emit another delegate request.",
    );
    message
}

fn build_hyard_continuation_hint(cwd: &Path) -> Option<String> {
    let config = SwitchyardConfig::resolve(cwd).unwrap_or_default();
    build_hyard_continuation_hint_from_dir(&config.job_dir(cwd))
}

async fn build_hyard_continuation_hint_async(cwd: PathBuf) -> Option<String> {
    // Listing HYARD manifests can touch the filesystem and, for stale-job
    // recovery, probe OS process state. Keep it away from the async reactor and
    // cap how long prompt decoration can delay the user's turn. If this best
    // effort hint times out, the durable job state is still available through
    // explicit HYARD status/result calls and the runtime DB/UI snapshot.
    let task = tokio::task::spawn_blocking(move || build_hyard_continuation_hint(&cwd));
    match tokio::time::timeout(std::time::Duration::from_millis(750), task).await {
        Ok(Ok(hint)) => hint,
        Ok(Err(_)) | Err(_) => None,
    }
}

fn build_hyard_session_hint(session_id: Uuid) -> String {
    format!(
        "HYARD live-session hint:\n\
         - Current Switchyard session id: {session_id}\n\
         - If you launch background HYARD work, pass `--session {session_id}` to `/hyard:delegate` so callback receipts return to this live session inbox.\n\
         - Reuse the same session id with `/hyard:watch --session {session_id} ...`, `/hyard:inbox --session {session_id} ...`, `/hyard:resume --session {session_id} --callbacks`, or the single-call watcher `/hyard:follow --session {session_id} ...`."
    )
}

fn build_hyard_continuation_hint_from_dir(job_dir: &Path) -> Option<String> {
    let mut jobs = list_job_summaries(job_dir, MAX_CONTINUATION_HINT_JOBS * 3)
        .into_iter()
        .filter(|job| is_active_hyard_job_status(&job.status))
        .collect::<Vec<_>>();

    if jobs.is_empty() {
        return None;
    }

    jobs.sort_by(|left, right| {
        right
            .wait_timeout_count
            .cmp(&left.wait_timeout_count)
            .then_with(|| {
                hyard_job_status_rank(&right.status).cmp(&hyard_job_status_rank(&left.status))
            })
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.provider.cmp(&right.provider))
    });
    jobs.truncate(MAX_CONTINUATION_HINT_JOBS);

    let mut lines = vec![
        "HYARD continuation hint:".to_string(),
        "You already have live HYARD job(s):".to_string(),
    ];
    for job in jobs {
        let mut detail = format!(
            "- provider={} job_id={} status={}",
            job.provider, job.job_id, job.status
        );
        if job.wait_timeout_count > 0 {
            detail.push_str(&format!(" wait_timeouts={}", job.wait_timeout_count));
        }
        if let Some(last_event) = job.last_event.as_deref().and_then(compact_hyard_detail) {
            detail.push_str(&format!(" last_event={last_event}"));
        } else if let Some(preview) = job
            .last_output_preview
            .as_deref()
            .and_then(compact_hyard_detail)
        {
            detail.push_str(&format!(" preview={preview}"));
        }
        lines.push(detail);
    }
    lines.push(
        "Treat HYARD as a background tool: complex LLM jobs can outlast a short wait window, and a running job_id is still useful work in flight."
            .to_string(),
    );
    lines.push(
        "If a previous HYARD bridge call returned wait_timeout, that is not a failure; the same job_id is still running in background."
            .to_string(),
    );
    lines.push(
        "Prefer /hyard:status <job-id>, /hyard:result <job-id>, or /hyard:await <job-id> --timeout-sec <n> before starting a fresh delegate."
            .to_string(),
    );
    lines.push(
        "Do not call /hyard:await immediately after /hyard:delegate unless the very next step is blocked on that peer result."
            .to_string(),
    );
    lines.push(
        "You may run multiple independent HYARD jobs in parallel when their tasks do not overlap. Continue other useful work while they are in flight."
            .to_string(),
    );
    lines.push(
        "Do not restart the same delegate from scratch when you already have a job_id.".to_string(),
    );
    Some(lines.join("\n"))
}

fn collect_callback_receipts_for_injection(
    store: &(impl CanonicalStore + ?Sized),
    session_id: Uuid,
) -> Result<Vec<InboxEntry>, CoreError> {
    let mut entries = store
        .list_inbox_entries(session_id)?
        .into_iter()
        .filter(|entry| {
            entry.is_unread() && !matches!(entry.delivery_mode(), InboxDeliveryMode::Quiet)
        })
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| {
        callback_delivery_rank(right.delivery_mode())
            .cmp(&callback_delivery_rank(left.delivery_mode()))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.entry_id.cmp(&right.entry_id))
    });

    Ok(entries)
}

fn callback_delivery_rank(mode: InboxDeliveryMode) -> u8 {
    match mode {
        InboxDeliveryMode::Immediate => 2,
        InboxDeliveryMode::Checkpoint => 1,
        InboxDeliveryMode::Quiet => 0,
        _ => 0,
    }
}

fn consume_callback_receipts(
    store: &mut (impl CanonicalStore + ?Sized),
    receipts: &mut [InboxEntry],
) -> Result<(), CoreError> {
    for entry in receipts {
        entry.mark_consumed();
        store.save_inbox_entry(entry)?;
    }
    Ok(())
}

fn load_turn_status(
    store: &(impl CanonicalStore + ?Sized),
    session_id: Uuid,
    turn_id: Uuid,
) -> Result<TurnStatus, CoreError> {
    store
        .list_turns(session_id)?
        .into_iter()
        .find(|turn| turn.turn_id == turn_id)
        .map(|turn| turn.status)
        .ok_or_else(|| CoreError::Runner(format!("turn '{turn_id}' not found after execution")))
}

fn build_callback_receipt_hint(entries: &[InboxEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }

    let mut lines = vec![
        "BACKGROUND COMPLETION NOTICES:".to_string(),
        "These are runtime callback receipts from background HYARD jobs that finished since your last turn.".to_string(),
        "Treat them as background completion context, not as a new user request.".to_string(),
    ];

    for entry in entries {
        let mut detail = format!("- [{}] kind={}", entry.delivery_mode(), entry.kind);

        if let Some(provider) = entry.provider.as_deref() {
            detail.push_str(&format!(" provider={provider}"));
        }
        if let Some(job_id) = entry.job_id {
            detail.push_str(&format!(" job_id={job_id}"));
        }
        if let Some(job_status) = entry
            .payload
            .get("job_status")
            .and_then(|value| value.as_str())
        {
            detail.push_str(&format!(" status={job_status}"));
        }
        if !entry.title.trim().is_empty() {
            detail.push_str(&format!(
                " title={}",
                preview_collapsed(&entry.title, 80, "…")
            ));
        }
        lines.push(detail);

        if let Some(preview) = entry
            .summary
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| (!entry.message.trim().is_empty()).then_some(entry.message.as_str()))
        {
            lines.push(format!("  note: {}", preview_collapsed(preview, 120, "…")));
        }
    }

    lines.push(
        "Reuse the referenced job_id with /hyard:result or /hyard:status before launching the same delegate again."
            .to_string(),
    );
    Some(lines.join("\n"))
}

fn combine_hint_blocks<'a>(blocks: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    let blocks = blocks
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|block| !block.is_empty())
        .collect::<Vec<_>>();

    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
}

fn is_active_hyard_job_status(status: &str) -> bool {
    matches!(status, "queued" | "running" | "cancel_requested")
}

fn hyard_job_status_rank(status: &str) -> u8 {
    match status {
        "running" => 3,
        "cancel_requested" => 2,
        "queued" => 1,
        _ => 0,
    }
}

fn compact_hyard_detail(text: &str) -> Option<String> {
    let preview = preview_collapsed(text, 80, "…");
    (!preview.is_empty()).then_some(preview)
}

/// Run a routed turn: execute core, check for delegate, execute peer, finalize core.
///
/// This is the top-level entry point for CLI and TUI. It replaces direct
/// calls to `run_turn()` when orchestration is enabled.
#[allow(clippy::too_many_arguments)]
pub async fn run_routed_turn<S: CanonicalStore + ?Sized>(
    store: &mut S,
    session: &mut Session,
    core_provider: &dyn Provider,
    peer_catalog: &PeerCatalog,
    resolve_peer: &(dyn Fn(&str) -> Option<Box<dyn Provider>> + Send + Sync),
    registry: Option<std::sync::Arc<dyn LiveInstanceRegistry>>,
    user_message: String,
    cwd: PathBuf,
) -> Result<RoutedTurnOutput, CoreError> {
    run_routed_turn_with_archive(
        store,
        session,
        core_provider,
        peer_catalog,
        resolve_peer,
        registry,
        user_message,
        cwd,
        None,
    )
    .await
}

/// Routed turn with optional raw output archiving.
#[allow(clippy::too_many_arguments)]
pub async fn run_routed_turn_with_archive(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    core_provider: &dyn Provider,
    peer_catalog: &PeerCatalog,
    resolve_peer: &(dyn Fn(&str) -> Option<Box<dyn Provider>> + Send + Sync),
    registry: Option<std::sync::Arc<dyn LiveInstanceRegistry>>,
    user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
) -> Result<RoutedTurnOutput, CoreError> {
    let policy = ExecutionPolicy::workspace_write(cwd.clone());
    run_routed_turn_with_archive_and_policy(
        store,
        session,
        core_provider,
        peer_catalog,
        resolve_peer,
        registry,
        user_message,
        cwd,
        artifact_dir,
        policy,
    )
    .await
}

/// Routed turn with optional raw output archiving and explicit sandbox policy.
#[allow(clippy::too_many_arguments)]
pub async fn run_routed_turn_with_archive_and_policy(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    core_provider: &dyn Provider,
    peer_catalog: &PeerCatalog,
    resolve_peer: &(dyn Fn(&str) -> Option<Box<dyn Provider>> + Send + Sync),
    registry: Option<std::sync::Arc<dyn LiveInstanceRegistry>>,
    user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    policy: ExecutionPolicy,
) -> Result<RoutedTurnOutput, CoreError> {
    run_routed_turn_observable_with_policy(
        store,
        session,
        core_provider,
        peer_catalog,
        resolve_peer,
        registry,
        user_message,
        cwd,
        artifact_dir,
        None,
        switchyard_provider_api::CancellationToken::new(),
        policy,
    )
    .await
}

/// Full observable routed turn. Emits RuntimeEvent through `runtime_tx`
/// so the TUI can display live state without polling the store.
#[allow(clippy::too_many_arguments)]
pub async fn run_routed_turn_observable(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    core_provider: &dyn Provider,
    peer_catalog: &PeerCatalog,
    resolve_peer: &(dyn Fn(&str) -> Option<Box<dyn Provider>> + Send + Sync),
    registry: Option<std::sync::Arc<dyn LiveInstanceRegistry>>,
    user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&tokio::sync::mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
    cancel: switchyard_provider_api::CancellationToken,
) -> Result<RoutedTurnOutput, CoreError> {
    let policy = ExecutionPolicy::workspace_write(cwd.clone());
    run_routed_turn_observable_with_policy(
        store,
        session,
        core_provider,
        peer_catalog,
        resolve_peer,
        registry,
        user_message,
        cwd,
        artifact_dir,
        runtime_tx,
        cancel,
        policy,
    )
    .await
}

/// Full observable routed turn with explicit sandbox policy. Emits RuntimeEvent
/// through `runtime_tx` so the TUI can display live state without polling the
/// store.
#[allow(clippy::too_many_arguments)]
pub async fn run_routed_turn_observable_with_policy(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    core_provider: &dyn Provider,
    peer_catalog: &PeerCatalog,
    resolve_peer: &(dyn Fn(&str) -> Option<Box<dyn Provider>> + Send + Sync),
    registry: Option<std::sync::Arc<dyn LiveInstanceRegistry>>,
    user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&tokio::sync::mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
    cancel: switchyard_provider_api::CancellationToken,
    base_policy: ExecutionPolicy,
) -> Result<RoutedTurnOutput, CoreError> {
    run_routed_turn_observable_with_policy_and_attachments(
        store,
        session,
        core_provider,
        peer_catalog,
        resolve_peer,
        registry,
        user_message,
        Vec::new(),
        cwd,
        artifact_dir,
        runtime_tx,
        cancel,
        base_policy,
    )
    .await
}

/// Full observable routed turn with explicit sandbox policy and local
/// attachments. Attachments are delivered structurally to the first Core
/// provider turn; they are not rendered into the stored user message.
#[allow(clippy::too_many_arguments)]
pub async fn run_routed_turn_observable_with_policy_and_attachments(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    core_provider: &dyn Provider,
    peer_catalog: &PeerCatalog,
    resolve_peer: &(dyn Fn(&str) -> Option<Box<dyn Provider>> + Send + Sync),
    registry: Option<std::sync::Arc<dyn LiveInstanceRegistry>>,
    user_message: String,
    attachments: Vec<InputAttachment>,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&tokio::sync::mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
    cancel: switchyard_provider_api::CancellationToken,
    base_policy: ExecutionPolicy,
) -> Result<RoutedTurnOutput, CoreError> {
    run_routed_turn_observable_with_policy_attachments_and_prompt_injection(
        store,
        session,
        core_provider,
        peer_catalog,
        resolve_peer,
        registry,
        user_message,
        attachments,
        cwd,
        artifact_dir,
        runtime_tx,
        cancel,
        base_policy,
        RouterPromptInjection::VerboseOrchestration,
    )
    .await
}

/// Full observable routed turn with explicit sandbox policy, local
/// attachments, and a prompt-injection policy.
///
/// GUI chat should use [`RouterPromptInjection::Clean`] so user messages and
/// structured attachments are not polluted with Switchyard/HYARD debugging
/// hints. CLI/TUI agent flows can keep [`RouterPromptInjection::VerboseOrchestration`]
/// when they want model-driven delegation.
#[allow(clippy::too_many_arguments)]
pub async fn run_routed_turn_observable_with_policy_attachments_and_prompt_injection(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    core_provider: &dyn Provider,
    peer_catalog: &PeerCatalog,
    resolve_peer: &(dyn Fn(&str) -> Option<Box<dyn Provider>> + Send + Sync),
    registry: Option<std::sync::Arc<dyn LiveInstanceRegistry>>,
    user_message: String,
    attachments: Vec<InputAttachment>,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&tokio::sync::mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
    cancel: switchyard_provider_api::CancellationToken,
    base_policy: ExecutionPolicy,
    prompt_injection: RouterPromptInjection,
) -> Result<RoutedTurnOutput, CoreError> {
    let mut last_output: Option<TurnOutput> = None;
    let mut delegated = false;
    let mut inject_context: Option<String> = None;
    let user_turn_input = TurnInput {
        user_message,
        system_prompt: None,
        attachments: attachments.clone(),
    };
    let stored_user_message = user_turn_input.user_message_text();
    let (continuation_hint, initial_hint, mut callback_receipts) = if prompt_injection
        .includes_orchestration()
    {
        let continuation_hint = build_hyard_continuation_hint_async(cwd.clone()).await;
        let session_hint = build_hyard_session_hint(session.session_id);
        let callback_receipts = collect_callback_receipts_for_injection(store, session.session_id)?;
        let callback_hint = build_callback_receipt_hint(&callback_receipts);
        let initial_hint = combine_hint_blocks([
            Some(session_hint.as_str()),
            continuation_hint.as_deref(),
            callback_hint.as_deref(),
        ]);
        (continuation_hint, initial_hint, callback_receipts)
    } else {
        (None, None, Vec::new())
    };

    // Resolve config once for retry policy + per-provider env lookup. Helpers
    // that resolve their own copy (e.g. build_hyard_continuation_hint) keep
    // doing so for now — duplication is cheap, refactoring is out of scope.
    let routed_config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let retry_policy = Some(switchyard_orchestrator::RetryPolicy::from_config(
        &routed_config.orchestrator.worker_retry,
    ));
    let provider_envs: std::collections::HashMap<
        String,
        std::collections::HashMap<String, String>,
    > = routed_config
        .providers
        .iter()
        .map(|(name, pc)| (name.clone(), pc.env.clone()))
        .collect();

    for iteration in 0..MAX_ROUTER_LOOPS {
        let message = if iteration == 0 {
            // First iteration: clean user text, optionally augmented with
            // peer catalog + HYARD continuity state for orchestration mode.
            build_first_iteration_router_message(
                &stored_user_message,
                peer_catalog,
                initial_hint.as_deref(),
                prompt_injection,
            )
        } else if let Some(ref ctx) = inject_context {
            // Finalization: original task + delegate result + explicit instruction
            build_finalization_router_message(
                &stored_user_message,
                ctx,
                continuation_hint.as_deref(),
            )
        } else {
            break;
        };

        let phase = if iteration > 0 {
            crate::turn_runner::TurnPhase::Finalization
        } else {
            crate::turn_runner::TurnPhase::Normal
        };

        // Check cancellation before starting each iteration
        if cancel.is_cancelled() {
            return Err(CoreError::Runner("cancelled by user".to_string()));
        }

        if iteration == 0
            && !callback_receipts.is_empty()
            && let Some(tx) = runtime_tx
        {
            tx.send(
                crate::runtime_events::RuntimeEvent::CallbackReceiptsInjected {
                    session_id: session.session_id,
                    provider: session.active_core.clone(),
                    count: callback_receipts.len(),
                },
            )
            .await
            .ok();
        }

        let output = if iteration == 0 {
            crate::turn_runner::run_turn_phased_with_messages_policy_and_attachments(
                store,
                session,
                core_provider,
                stored_user_message.clone(),
                message,
                cwd.clone(),
                artifact_dir,
                runtime_tx,
                phase,
                cancel.clone(),
                attachments.clone(),
                base_policy.clone(),
            )
            .await
        } else {
            crate::turn_runner::run_turn_phased_with_policy(
                store,
                session,
                core_provider,
                message,
                cwd.clone(),
                artifact_dir,
                runtime_tx,
                phase,
                cancel.clone(),
                base_policy.clone(),
            )
            .await
        };
        let output = match output {
            Ok(output) => output,
            Err(err) => return Err(err),
        };
        let response_text = output.response.clone().unwrap_or_default();
        let core_turn_id = output.turn_id;
        last_output = Some(output);
        let turn_status = load_turn_status(store, session.session_id, core_turn_id)?;

        if matches!(turn_status, TurnStatus::Failed | TurnStatus::Cancelled) {
            let clean_response = last_output.as_ref().and_then(|output| {
                output.response.clone().map(|response| {
                    let stripped = strip_sentinel_blocks(&response);
                    if stripped.is_empty() {
                        response
                    } else {
                        stripped
                    }
                })
            });

            return Ok(RoutedTurnOutput {
                turn_id: core_turn_id,
                response: clean_response,
                delegated,
            });
        }

        // Check for sentinel delegate block in response
        let sentinel_blocks = extract_sentinel_blocks(&response_text);
        let delegate_request: Option<DelegateRequest> = sentinel_blocks
            .first()
            .and_then(|block| serde_json::from_str::<DelegateRequest>(block).ok())
            .filter(|req| req.request_type == "delegate");

        if let Some(request) = delegate_request {
            if iteration >= MAX_ROUTER_LOOPS - 1 {
                break;
            }

            if request.requests.is_empty() {
                break;
            }

            if let Some(tx) = runtime_tx {
                for task in &request.requests {
                    tx.send(crate::runtime_events::RuntimeEvent::DelegateRequested {
                        session_id: session.session_id,
                        core_turn_id,
                        peer: task.id.clone(),
                        role: task.role.to_string(),
                        task_summary: preview_chars(&task.task, 80, "…"),
                    })
                    .await
                    .ok();
                }
            }

            // Slice 4: peer pre-spawning moved into WorkerSupervisor, which is
            // invoked by execute_delegate per task. Router no longer pre-pops
            // workers — leaving the legacy block out also avoids the
            // LabelConflict that double-registration would cause.
            let _ = registry.as_ref();

            let mut all_resolved = true;
            for task in &request.requests {
                let has_registry_instance = registry
                    .as_ref()
                    .map(|r| r.has_live_instance(&task.provider, session.session_id))
                    .unwrap_or(false);
                let peer = resolve_peer(&task.provider);
                if peer.is_none() && !has_registry_instance {
                    inject_context = Some(format!(
                        "Delegate to '{}' failed: provider not available.",
                        task.provider
                    ));
                    all_resolved = false;
                    break;
                }
            }
            if !all_resolved {
                continue;
            }

            // Build observer to forward peer events as RuntimeEvents
            let observer: Option<Box<switchyard_orchestrator::PeerEventObserver>> =
                runtime_tx.map(|tx| {
                    let session_id = session.session_id;
                    // The peer observer is a synchronous callback invoked while
                    // orchestrator drains provider events. Keep the callback
                    // non-blocking, but use a bounded hop so a stalled GUI/IPC
                    // bridge cannot grow memory without limit. High-volume
                    // terminal/text deltas are lossy under sustained overflow;
                    // lifecycle/HYARD/control events are preserved.
                    let live_tx = spawn_runtime_event_forwarder(tx);
                    Box::new(move |pe: &switchyard_provider_api::ProviderEvent| {
                        let peer = pe.provider.clone();
                        let event = if let Some(execution) =
                            switchyard_provider_api::extract_execution_telemetry(&pe.payload)
                        {
                            Some(
                                crate::runtime_events::RuntimeEvent::PeerExecutionTelemetry {
                                    session_id,
                                    turn_id: pe.turn_id,
                                    provider: peer.clone(),
                                    execution,
                                },
                            )
                        } else if let Some(job) =
                            switchyard_provider_api::extract_hyard_job_observation(&pe.payload)
                        {
                            Some(crate::runtime_events::RuntimeEvent::HyardJobObserved {
                                session_id,
                                turn_id: pe.turn_id,
                                source_provider: pe.provider.clone(),
                                observed_at: pe.timestamp.to_rfc3339(),
                                job,
                            })
                        } else if let Some(terminal) =
                            switchyard_provider_api::extract_terminal_output(&pe.payload)
                        {
                            Some(crate::runtime_events::RuntimeEvent::PeerTerminalOutput {
                                session_id,
                                turn_id: pe.turn_id,
                                provider: peer.clone(),
                                text: terminal.line,
                                transport: terminal.transport,
                            })
                        } else {
                            match pe.event_type {
                                switchyard_provider_api::EventType::TurnStarted => {
                                    Some(crate::runtime_events::RuntimeEvent::PeerTurnStarted {
                                        session_id,
                                        turn_id: pe.turn_id,
                                        provider: peer.clone(),
                                    })
                                }
                                switchyard_provider_api::EventType::ItemStarted
                                | switchyard_provider_api::EventType::ItemUpdated
                                | switchyard_provider_api::EventType::ItemCompleted
                                | switchyard_provider_api::EventType::ArtifactReady => {
                                    if switchyard_provider_api::is_empty_reasoning_payload(
                                        &pe.payload,
                                    ) {
                                        None
                                    } else {
                                        let item_text = pe.display_text_or_summary();
                                        Some(crate::runtime_events::RuntimeEvent::PeerItemUpdated {
                                            session_id,
                                            turn_id: pe.turn_id,
                                            provider: peer.clone(),
                                            event_type: pe.event_type.to_string(),
                                            text: item_text.unwrap_or_default(),
                                            payload: Some(pe.payload.clone()),
                                        })
                                    }
                                }
                                switchyard_provider_api::EventType::TurnCompleted => {
                                    Some(crate::runtime_events::RuntimeEvent::PeerOutputCompleted {
                                        session_id,
                                        turn_id: pe.turn_id,
                                        provider: peer.clone(),
                                    })
                                }
                                _ => None,
                            }
                        };
                        if let Some(evt) = event {
                            try_forward_observed_runtime_event(&live_tx, evt);
                        }
                    }) as Box<switchyard_orchestrator::PeerEventObserver>
                });
            let obs_ref = observer.as_deref();

            // Wire the supervisor's lifecycle observer through the existing
            // runtime_tx so the GUI/TUI can drop its polling and react to
            // worker spawn/state/retry/terminate events in real time.
            let supervisor_observer: Option<
                std::sync::Arc<switchyard_orchestrator::supervisor::SupervisorObserver>,
            > = runtime_tx.map(|tx| {
                let live_tx = spawn_runtime_event_forwarder(tx);
                std::sync::Arc::new(
                    move |event: switchyard_orchestrator::supervisor::SupervisorLifecycleEvent| {
                        let runtime_event = match event {
                            switchyard_orchestrator::supervisor::SupervisorLifecycleEvent::Spawned {
                                session_id,
                                instance_id,
                                provider,
                                label,
                                kind,
                                spawned_at,
                            } => crate::runtime_events::RuntimeEvent::WorkerSpawned {
                                session_id,
                                instance_id,
                                provider,
                                label,
                                kind,
                                spawned_at,
                            },
                            switchyard_orchestrator::supervisor::SupervisorLifecycleEvent::StateChanged {
                                session_id,
                                instance_id,
                                state,
                                in_flight_turn_id,
                            } => crate::runtime_events::RuntimeEvent::WorkerStateChanged {
                                session_id,
                                instance_id,
                                state,
                                in_flight_turn_id,
                            },
                            switchyard_orchestrator::supervisor::SupervisorLifecycleEvent::Retrying {
                                session_id,
                                instance_id,
                                provider,
                                label,
                                attempt,
                                last_error,
                            } => crate::runtime_events::RuntimeEvent::WorkerRetrying {
                                session_id,
                                instance_id,
                                provider,
                                label,
                                attempt,
                                last_error,
                            },
                            switchyard_orchestrator::supervisor::SupervisorLifecycleEvent::Terminated {
                                session_id,
                                instance_id,
                                provider,
                                label,
                                reason,
                            } => crate::runtime_events::RuntimeEvent::WorkerTerminated {
                                session_id,
                                instance_id,
                                provider,
                                label,
                                reason,
                            },
                        };
                        try_forward_observed_runtime_event(&live_tx, runtime_event);
                    },
                )
                    as std::sync::Arc<
                        switchyard_orchestrator::supervisor::SupervisorObserver,
                    >
            });

            // Execute delegate through orchestrator. retry_policy + per-
            // provider env are derived from SwitchyardConfig at the top of
            // this function so each iteration shares the same view.
            match switchyard_orchestrator::execute_delegate(
                &request,
                session,
                store,
                peer_catalog,
                resolve_peer,
                registry.clone(),
                retry_policy.clone(),
                provider_envs.clone(),
                supervisor_observer,
                core_turn_id,
                obs_ref,
                cancel.clone(),
            )
            .await
            {
                Ok(response) => {
                    delegated = true;
                    if let Some(tx) = runtime_tx {
                        for result in &response.results {
                            let status = result.status.to_string();
                            tx.send(crate::runtime_events::RuntimeEvent::DelegateCompleted {
                                session_id: session.session_id,
                                core_turn_id,
                                peer: result.id.clone(),
                                status,
                                summary: result.summary.clone(),
                            })
                            .await
                            .ok();
                        }
                    }
                    let result_block = render_delegate_result_block(&response.results);
                    inject_context = Some(result_block);
                }
                Err(e) => {
                    if let Some(tx) = runtime_tx {
                        for task in &request.requests {
                            tx.send(crate::runtime_events::RuntimeEvent::DelegateCompleted {
                                session_id: session.session_id,
                                core_turn_id,
                                peer: task.id.clone(),
                                status: "failed".to_string(),
                                summary: Some(e.to_string()),
                            })
                            .await
                            .ok();
                        }
                    }
                    let failed_peers = request
                        .requests
                        .iter()
                        .map(|t| t.provider.clone())
                        .collect::<Vec<_>>()
                        .join(", ");
                    inject_context = Some(format!("Delegate to '{}' failed: {e}", failed_peers));
                }
            }
        } else {
            // No delegate — done
            break;
        }
    }

    let output = match last_output {
        Some(output) => output,
        None => return Err(CoreError::Runner("no turn executed".to_string())),
    };

    if !callback_receipts.is_empty()
        && let Err(err) = consume_callback_receipts(store, &mut callback_receipts)
    {
        eprintln!("warning: failed to persist consumed callback receipts: {err}");
    }

    // Strip sentinel blocks from the final response so users see clean prose.
    let clean_response = output.response.map(|r| {
        let stripped = strip_sentinel_blocks(&r);
        if stripped.is_empty() { r } else { stripped }
    });

    Ok(RoutedTurnOutput {
        turn_id: output.turn_id,
        response: clean_response,
        delegated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use switchyard_provider_api::{PeerDescriptor, ProviderCapability, ProviderRole};

    fn sample_peer_catalog() -> PeerCatalog {
        let mut catalog = PeerCatalog::new();
        catalog.add(PeerDescriptor {
            provider_id: "claude".to_string(),
            roles: vec![ProviderRole::Reviewer],
            available: true,
            capabilities: vec![ProviderCapability::HeadlessTurn],
            description: "Claude CLI".to_string(),
            host_surface: None,
        });
        catalog
    }

    #[test]
    fn clean_prompt_injection_keeps_first_turn_to_user_text_only() {
        let message = build_first_iteration_router_message(
            "图片输入测试",
            &sample_peer_catalog(),
            Some("HYARD live-session hint:\n- Current Switchyard session id: test"),
            RouterPromptInjection::Clean,
        );

        assert_eq!(message, "图片输入测试");
        assert!(!message.contains("Available peer providers"));
        assert!(!message.contains("SWITCHYARD_JSON_BEGIN"));
        assert!(!message.contains("HYARD live-session hint"));
    }

    #[test]
    fn verbose_prompt_injection_keeps_legacy_orchestration_hints() {
        let message = build_first_iteration_router_message(
            "实现这个功能",
            &sample_peer_catalog(),
            Some("HYARD live-session hint:\n- Current Switchyard session id: test"),
            RouterPromptInjection::VerboseOrchestration,
        );

        assert!(message.contains("实现这个功能"));
        assert!(message.contains("Available peer providers"));
        assert!(message.contains("SWITCHYARD_JSON_BEGIN"));
        assert!(message.contains("HYARD live-session hint"));
    }

    #[test]
    fn continuation_hint_reads_active_jobs_and_prioritizes_wait_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let job_dir = dir.path();
        fs::write(
            job_dir.join("completed.json"),
            r#"{
                "job_id": "00000000-0000-0000-0000-000000000001",
                "provider": "gemini",
                "status": "completed",
                "updated_at": "2026-04-04T12:00:00Z"
            }"#,
        )
        .unwrap();
        fs::write(
            job_dir.join("queued.json"),
            r#"{
                "job_id": "00000000-0000-0000-0000-000000000002",
                "provider": "codex",
                "status": "queued",
                "updated_at": "2026-04-04T09:00:00Z"
            }"#,
        )
        .unwrap();
        fs::write(
            job_dir.join("running.json"),
            r#"{
                "job_id": "00000000-0000-0000-0000-000000000003",
                "provider": "claude",
                "status": "running",
                "updated_at": "2026-04-04T11:00:00Z",
                "last_event": "item_updated:claude",
                "wait_timeout_count": 2
            }"#,
        )
        .unwrap();

        let hint = build_hyard_continuation_hint_from_dir(job_dir).unwrap();
        assert!(hint.contains("HYARD continuation hint"));
        assert!(hint.contains("wait_timeout"));
        assert!(hint.contains("background tool"));
        assert!(hint.contains("multiple independent HYARD jobs"));
        assert!(hint.contains("00000000-0000-0000-0000-000000000003"));
        assert!(!hint.contains("00000000-0000-0000-0000-000000000001"));

        let claude_pos = hint
            .find("provider=claude job_id=00000000-0000-0000-0000-000000000003")
            .unwrap();
        let codex_pos = hint
            .find("provider=codex job_id=00000000-0000-0000-0000-000000000002")
            .unwrap();
        assert!(
            claude_pos < codex_pos,
            "running wait-timeout job should sort first"
        );
    }

    #[test]
    fn finalization_message_includes_continuation_hint() {
        let message = build_finalization_router_message(
            "original task",
            "delegate_result payload",
            Some("HYARD continuation hint:\n- provider=claude job_id=abc status=running"),
        );

        assert!(message.contains("Original user task:\noriginal task"));
        assert!(message.contains("delegate_result payload"));
        assert!(message.contains("HYARD continuation hint"));
        assert!(message.contains("Do NOT emit another delegate request."));
    }

    #[test]
    fn preview_chars_is_utf8_safe_for_multibyte_delegate_tasks() {
        let text = "这是一次通过 hyard/Switchyard 发起的最小连通性测试。请简短回复以下三项：1）已成功收到任务；2）你的 provider 与 role；3）输出字符串 pong。";
        let truncated = preview_chars(text, 80, "…");

        assert!(truncated.starts_with("这是一次通过"));
        assert!(truncated.ends_with('…'));
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn callback_receipt_hint_marks_runtime_callback_context() {
        let session_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();
        let mut entry = InboxEntry::background_job_receipt(
            session_id,
            "claude",
            "Claude background job completed",
            "Claude finished a review while you were idle.",
        );
        entry.job_id = Some(job_id);
        entry.summary = Some("Found one follow-up item.".to_string());
        entry.payload = serde_json::json!({ "job_status": "completed" });

        let hint = build_callback_receipt_hint(&[entry]).expect("callback hint");
        assert!(hint.contains("BACKGROUND COMPLETION NOTICES"));
        assert!(hint.contains("runtime callback receipts"));
        assert!(hint.contains("not as a new user request"));
        assert!(hint.contains(&job_id.to_string()));
        assert!(hint.contains("Reuse the referenced job_id"));
    }
}
