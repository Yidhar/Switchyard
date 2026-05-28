//! Persistent Codex instance driven via `codex app-server` JSON-RPC over stdio.
//!
//! Wire protocol (verified by the `app_server_spike` example):
//!
//! ```text
//! → initialize { clientInfo }
//! ← { codexHome, platformFamily, userAgent, ... }
//! → initialized (notification, no id)
//! → thread/start {}
//! ← { thread: { id, sessionId, path, ... } }       (thread_id captured)
//! → turn/start { threadId, input: [{type:"text", text:"..."}] }
//! ← { turn: { id, status:"inProgress" } }
//! ⤵ item/started, item/completed, item/agentMessage/delta, turn/started,
//!   thread/status/changed, mcpServer/startupStatus/updated,
//!   thread/tokenUsage/updated, account/rateLimits/updated, ...
//! ⤵ turn/completed { threadId, turn: { id, completedAt, durationMs, error } }
//! ```
//!
//! Each `send_message` writes one `turn/start`; the spawned drain task forwards
//! item-level notifications as Switchyard `ProviderEvent`s and closes the
//! channel on the `turn/completed` (or `turn/failed`) notification.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use switchyard_provider_api::{
    ContextBundle, EffectiveSandboxMode, EventType, ExecutionPolicy, LiveInstance, ProviderError,
    ProviderEvent, TurnInput,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);

type PendingApprovalMap = HashMap<String, oneshot::Sender<ResolvedApproval>>;

static PENDING_APPROVALS: OnceLock<Arc<Mutex<PendingApprovalMap>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalDecision {
    Approve,
    Deny,
}

impl ToolApprovalDecision {
    fn codex_result(self) -> Value {
        match self {
            ToolApprovalDecision::Approve => json!({ "decision": "approve" }),
            ToolApprovalDecision::Deny => json!({ "decision": "deny" }),
        }
    }

    fn audit_tag(self) -> &'static str {
        match self {
            ToolApprovalDecision::Approve => "approve:user",
            ToolApprovalDecision::Deny => "deny:user",
        }
    }
}

#[derive(Debug)]
struct ResolvedApproval {
    decision: ToolApprovalDecision,
    reason: Option<String>,
}

#[derive(Debug)]
struct ApprovalResolution {
    decision: Value,
    audit_tag: String,
    reason: Option<String>,
    status: &'static str,
}

fn approval_registry() -> &'static Arc<Mutex<PendingApprovalMap>> {
    PENDING_APPROVALS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

async fn register_pending_approval(request_id: String) -> oneshot::Receiver<ResolvedApproval> {
    let (tx, rx) = oneshot::channel();
    approval_registry().lock().await.insert(request_id, tx);
    rx
}

async fn forget_pending_approval(request_id: &str) {
    approval_registry().lock().await.remove(request_id);
}

/// Resolve a pending Codex app-server approval request.
///
/// The GUI calls this through a Tauri command after rendering the structured
/// `approval_request` item from the session stream. The matching drain task is
/// blocked on a one-shot receiver and will forward the selected decision back
/// to Codex's JSON-RPC request.
pub async fn submit_tool_approval_decision(
    request_id: &str,
    decision: ToolApprovalDecision,
    reason: Option<String>,
) -> Result<(), String> {
    let sender = {
        let mut registry = approval_registry().lock().await;
        match registry.remove(request_id) {
            Some(sender) => sender,
            None => {
                let mut pending_sample = registry.keys().take(5).cloned().collect::<Vec<_>>();
                pending_sample.sort();
                return Err(format!(
                    "approval request '{request_id}' not found; it may have already been resolved, timed out, or replaced by a newer turn; pending={}; pending_sample={pending_sample:?}",
                    registry.len()
                ));
            }
        }
    };
    sender
        .send(ResolvedApproval { decision, reason })
        .map_err(|_| "approval request receiver is no longer active".to_string())
}

async fn wait_for_gui_approval(
    request_id: &str,
    approval_rx: oneshot::Receiver<ResolvedApproval>,
) -> ApprovalResolution {
    tokio::select! {
        resolved = approval_rx => match resolved {
            Ok(resolved) => ApprovalResolution {
                decision: resolved.decision.codex_result(),
                audit_tag: resolved.decision.audit_tag().to_string(),
                reason: resolved.reason,
                status: if resolved.decision == ToolApprovalDecision::Approve {
                    "completed"
                } else {
                    "failed"
                },
            },
            Err(_closed) => ApprovalResolution {
                decision: json!({ "decision": "deny" }),
                audit_tag: "deny:approval_channel_closed".to_string(),
                reason: Some(
                    "approval request was canceled before a GUI decision arrived".to_string(),
                ),
                status: "failed",
            },
        },
        _ = sleep(APPROVAL_TIMEOUT) => {
            forget_pending_approval(request_id).await;
            ApprovalResolution {
                decision: json!({ "decision": "deny" }),
                audit_tag: "deny:timeout_waiting_for_user".to_string(),
                reason: Some(format!(
                    "timed out after {} seconds waiting for approve/deny",
                    APPROVAL_TIMEOUT.as_secs()
                )),
                status: "failed",
            }
        },
    }
}

/// Long-running Codex daemon spoken to over JSON-RPC 2.0 NDJSON on stdio.
pub struct CodexAppServerInstance {
    pub child: Child,
    /// Wrapped in `Arc<Mutex>` so the per-turn drain task can reply to
    /// server-initiated requests (approval prompts) without contending with
    /// the caller of `send_message`.
    pub stdin: Arc<Mutex<ChildStdin>>,
    pub thread_id: String,
    /// Shared receiver of incoming JSON-RPC frames. Wrapped in `Mutex` so
    /// per-turn drain tasks can take exclusive ownership for their duration.
    pub frame_rx: Arc<Mutex<mpsc::Receiver<Value>>>,
    pub next_id: Arc<AtomicI64>,
}

impl CodexAppServerInstance {
    /// Spawn `codex app-server` and mint a fresh thread. Equivalent to
    /// `spawn_with_resume(..., None)`. Kept as a wrapper for ergonomics and
    /// to preserve the simpler call-site shape used by the integration tests.
    pub async fn spawn(
        command: &str,
        extra_args: &[String],
        env: HashMap<String, String>,
        cwd: Option<&Path>,
    ) -> Result<Self, ProviderError> {
        Self::spawn_with_resume(command, extra_args, env, cwd, None).await
    }

    /// Spawn `codex app-server` and optionally resume an existing thread.
    ///
    /// When `resume_thread_id` is `Some(id)` the spawn handshake tries
    /// `thread/resume` first; on any JSON-RPC error reply (unknown method,
    /// expired thread, …) it transparently falls back to `thread/start`,
    /// which means a stale or unsupported resume token always yields a
    /// usable instance — just one without the warm context.
    ///
    /// Read the resulting [`thread_id`](Self::thread_id) (or call
    /// [`LiveInstance::resume_token`]) after spawn to learn whether the
    /// resume took. Callers persist the post-spawn id so the *next* respawn
    /// can try resume again.
    pub async fn spawn_with_resume(
        command: &str,
        extra_args: &[String],
        env: HashMap<String, String>,
        cwd: Option<&Path>,
        resume_thread_id: Option<&str>,
    ) -> Result<Self, ProviderError> {
        let mut args: Vec<String> = vec!["app-server".to_string()];
        args.extend_from_slice(extra_args);

        let mut cmd = Command::new(command);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(d) = cwd {
            cmd.current_dir(d);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| {
            ProviderError::ExecutionFailed(format!("failed to spawn codex app-server: {e}"))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            ProviderError::ExecutionFailed("codex app-server: stdin missing after spawn".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ProviderError::ExecutionFailed("codex app-server: stdout missing after spawn".into())
        })?;
        let stderr = child.stderr.take();

        // Drain stderr so we never block on a full pipe.
        if let Some(se) = stderr {
            tokio::spawn(async move {
                let mut reader = BufReader::new(se).lines();
                while let Ok(Some(_line)) = reader.next_line().await {}
            });
        }

        // Bridge stdout NDJSON frames into a channel.
        let (frame_tx, frame_rx) = mpsc::channel::<Value>(1024);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && frame_tx.send(v).await.is_err()
                {
                    break;
                }
            }
        });

        let next_id = Arc::new(AtomicI64::new(0));
        let frame_rx = Arc::new(Mutex::new(frame_rx));
        let stdin = Arc::new(Mutex::new(stdin));

        // 1. initialize
        let init_id = next_id.fetch_add(1, Ordering::SeqCst) + 1;
        write_frame_arc(
            &stdin,
            json!({
                "jsonrpc": "2.0",
                "id": init_id,
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "switchyard",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }
            }),
        )
        .await
        .map_err(|e| ProviderError::ExecutionFailed(format!("initialize write: {e}")))?;

        let _init_reply = await_response(&frame_rx, init_id).await?;

        // 2. notify initialized
        write_frame_arc(
            &stdin,
            json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "params": {}
            }),
        )
        .await
        .map_err(|e| ProviderError::ExecutionFailed(format!("initialized notify write: {e}")))?;

        // 3. thread/resume (when we have a token) → thread/start (always
        //    available). A resume that errors out on the daemon side drops
        //    us back to a clean thread/start — the upper layer can detect
        //    via post-spawn `resume_token()` whether the token "took".
        let thread_id = match resume_thread_id {
            Some(prior_id) => {
                let resume_id = next_id.fetch_add(1, Ordering::SeqCst) + 1;
                write_frame_arc(
                    &stdin,
                    json!({
                        "jsonrpc": "2.0",
                        "id": resume_id,
                        "method": "thread/resume",
                        "params": { "threadId": prior_id }
                    }),
                )
                .await
                .map_err(|e| ProviderError::ExecutionFailed(format!("thread/resume write: {e}")))?;

                let resume_reply = await_response(&frame_rx, resume_id).await?;
                let resumed = resume_reply
                    .get("result")
                    .and_then(|r| r.get("thread"))
                    .and_then(|t| t.get("id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                match resumed {
                    Some(id) => id,
                    None => fresh_thread_start(&stdin, &frame_rx, &next_id).await?,
                }
            }
            None => fresh_thread_start(&stdin, &frame_rx, &next_id).await?,
        };

        Ok(Self {
            child,
            stdin,
            thread_id,
            frame_rx,
            next_id,
        })
    }
}

#[async_trait]
impl LiveInstance for CodexAppServerInstance {
    async fn send_message(
        &mut self,
        text: &str,
    ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
        // No policy supplied → permissive auto-approve. Preserves the
        // pre-Slice-C behavior for callers (smoke tests, supervisor's
        // delegate worker path) that don't carry an `ExecutionPolicy`.
        // User-facing flows go through `send_message_with_policy` so the
        // policy gates server-initiated approvals.
        self.send_message_with_policy(text, &ExecutionPolicy::permissive())
            .await
    }

    async fn send_message_with_policy(
        &mut self,
        text: &str,
        policy: &ExecutionPolicy,
    ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
        let input = TurnInput::text(text);
        self.send_turn_with_policy(&input, policy).await
    }

    async fn send_turn_with_policy(
        &mut self,
        input: &TurnInput,
        policy: &ExecutionPolicy,
    ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
        let policy = policy.clone();
        let input_items = turn_input_items(input);
        // Send turn/start. Do not synchronously wait for its response before
        // installing the drain task: newer Codex app-server builds can emit
        // notifications or server-initiated approval requests immediately
        // after `turn/start`. If we waited here, those frames would either be
        // discarded by `await_response` or deadlock the turn before the GUI had
        // an event receiver to render the pending approval card.
        let req_id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let turn_params = turn_start_params(&self.thread_id, input_items, &policy);
        write_frame_arc(
            &self.stdin,
            json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "method": "turn/start",
                "params": turn_params,
            }),
        )
        .await
        .map_err(|e| ProviderError::ExecutionFailed(format!("turn/start write: {e}")))?;

        let (event_tx, event_rx) = mpsc::channel::<ProviderEvent>(4096);
        let rx_lock = Arc::clone(&self.frame_rx);
        // Approval responses (Slice H) need to land on the same stdin the
        // turn/start invocation used. We share the lock — the per-frame write
        // is sub-millisecond so contention with the caller is negligible.
        let stdin_lock = Arc::clone(&self.stdin);
        // Switchyard's view: each call gets a fresh logical turn_id, distinct
        // from the codex-server-side turn id we filter by.
        let canonical_turn_id = Uuid::now_v7();
        let turn_start_req_id = Value::from(req_id);
        let stream_debug = debug_codex_stream_enabled();

        tokio::spawn(async move {
            let mut rx = rx_lock.lock().await;
            let mut server_turn_id: Option<String> = None;
            while let Some(frame) = rx.recv().await {
                // Server-initiated request — codex daemon asking us to
                // approve a tool/file action. User-facing flows surface this
                // as a structured pending item in the session stream and then
                // block here until the GUI sends approve/deny back through the
                // approval registry. The legacy no-policy `send_message()`
                // path still auto-approves to avoid hanging smoke tests and
                // background worker callers that do not have a GUI.
                if let (Some(req_id_val), Some(method_val)) = (
                    frame.get("id").cloned(),
                    frame.get("method").and_then(|m| m.as_str()),
                ) {
                    let method = method_val.to_string();
                    let params = frame
                        .get("params")
                        .cloned()
                        .unwrap_or_else(|| Value::Object(Default::default()));

                    if !is_approval_method(&method) {
                        let server_request_event = ProviderEvent::new(
                            canonical_turn_id,
                            EventType::ItemUpdated,
                            "codex",
                            json!({
                                "item_type": "server_request",
                                "method": method.clone(),
                                "status": "completed",
                                "rpc_id": req_id_val.clone(),
                                "request": params.clone(),
                                "summary": format!("Codex server request: {method}"),
                                "resolved_at_ms": unix_epoch_millis(),
                            }),
                        );
                        if event_tx.send(server_request_event).await.is_err() {
                            return;
                        }

                        let reply = json!({
                            "jsonrpc": "2.0",
                            "id": req_id_val,
                            "result": {},
                        });
                        if let Err(e) = write_frame_arc(&stdin_lock, reply).await {
                            let _ = event_tx
                                .send(ProviderEvent::turn_failed(
                                    canonical_turn_id,
                                    "codex",
                                    format!("server request response write failed: {e}"),
                                ))
                                .await;
                            return;
                        }
                        continue;
                    }

                    let request_id = approval_request_id(canonical_turn_id, &req_id_val, &method);
                    let (policy_decision, policy_tag) =
                        approval_decision(&method, &params, &policy);

                    let resolution = if is_legacy_permissive_policy(&policy) {
                        ApprovalResolution {
                            decision: policy_decision,
                            audit_tag: policy_tag.to_string(),
                            reason: Some(
                                "legacy non-interactive Codex call used permissive auto-approval"
                                    .to_string(),
                            ),
                            status: "completed",
                        }
                    } else {
                        let approval_rx = register_pending_approval(request_id.clone()).await;
                        let pending_event = ProviderEvent::new(
                            canonical_turn_id,
                            EventType::ItemUpdated,
                            "codex",
                            json!({
                                "item_type": "approval_request",
                                "request_id": request_id.clone(),
                                "rpc_id": req_id_val.clone(),
                                "method": method.clone(),
                                "status": "pending",
                                "request": params.clone(),
                                "policy": approval_policy_payload(&policy),
                                "policy_decision": {
                                    "decision_tag": policy_tag,
                                    "decision": policy_decision.clone(),
                                },
                                "timeout_secs": APPROVAL_TIMEOUT.as_secs(),
                                "created_at_ms": unix_epoch_millis(),
                                "summary": "Tool permission approval is waiting for the user",
                            }),
                        );

                        if event_tx.send(pending_event).await.is_err() {
                            forget_pending_approval(&request_id).await;
                            ApprovalResolution {
                                decision: json!({ "decision": "deny" }),
                                audit_tag: "deny:approval_stream_closed".to_string(),
                                reason: Some(
                                    "approval request could not be delivered to the session stream"
                                        .to_string(),
                                ),
                                status: "failed",
                            }
                        } else {
                            wait_for_gui_approval(&request_id, approval_rx).await
                        }
                    };

                    let _ = event_tx
                        .send(ProviderEvent::new(
                            canonical_turn_id,
                            EventType::ItemUpdated,
                            "codex",
                            json!({
                                "item_type": "approval_decision",
                                "request_id": request_id,
                                "rpc_id": req_id_val.clone(),
                                "method": method,
                                "status": resolution.status,
                                "decision_tag": resolution.audit_tag,
                                "decision": resolution.decision.clone(),
                                "request": params,
                                "reason": resolution.reason,
                                "policy": approval_policy_payload(&policy),
                                "resolved_at_ms": unix_epoch_millis(),
                            }),
                        ))
                        .await;

                    let reply = json!({
                        "jsonrpc": "2.0",
                        "id": req_id_val,
                        "result": resolution.decision,
                    });
                    if let Err(e) = write_frame_arc(&stdin_lock, reply).await {
                        // Best-effort: if we can't write the approval, the
                        // turn is going to stall anyway. Surface as a
                        // turn_failed and bail.
                        let _ = event_tx
                            .send(ProviderEvent::turn_failed(
                                canonical_turn_id,
                                "codex",
                                format!("approval response write failed: {e}"),
                            ))
                            .await;
                        return;
                    }
                    continue;
                }

                // Response to our outbound `turn/start` request. It can race
                // with notifications and approval requests, so the same drain
                // task that forwards session stream events also records it.
                if frame.get("id") == Some(&turn_start_req_id) {
                    if let Some(err) = frame.get("error") {
                        let _ = event_tx
                            .send(ProviderEvent::turn_failed(
                                canonical_turn_id,
                                "codex",
                                format!("turn/start rejected: {err}"),
                            ))
                            .await;
                        return;
                    }
                    if let Some(tid) = response_turn_id(&frame) {
                        server_turn_id = Some(tid.to_string());
                    }
                    continue;
                }

                let method = match frame.get("method").and_then(|m| m.as_str()) {
                    Some(m) => m,
                    None => continue, // unmatched response
                };

                // Filter to this turn (when params carry turnId).
                if let Some(tid) = notification_turn_id(&frame) {
                    if let Some(expected) = server_turn_id.as_deref() {
                        if tid != expected {
                            if stream_debug {
                                eprintln!(
                                    "[switchyard codex stream] drop method={method} server_turn_id={tid} expected={expected}"
                                );
                            }
                            continue;
                        }
                    } else if should_bind_server_turn_id(method) {
                        // Some app-server versions emit turn notifications
                        // before replying to `turn/start`. Only bind from
                        // turn-scoped notifications; process/status frames can
                        // carry unrelated ids and would otherwise make us drop
                        // the real Codex turn stream.
                        server_turn_id = Some(tid.to_string());
                    }
                }
                if stream_debug {
                    debug_codex_stream_notification(
                        method,
                        frame.get("params").unwrap_or(&Value::Null),
                        server_turn_id.as_deref(),
                        canonical_turn_id,
                    );
                }

                match method {
                    method if is_text_delta_method(method) => {
                        let params = frame.get("params").unwrap_or(&Value::Null);
                        if let Some(delta) = extract_codex_text_delta(method, params)
                            && event_tx
                                .send(ProviderEvent::text_message(
                                    canonical_turn_id,
                                    "codex",
                                    delta,
                                ))
                                .await
                                .is_err()
                        {
                            return;
                        }
                    }
                    "item/started" | "item/completed" | "item/updated" => {
                        // Pass through the item event with raw payload so
                        // canonical session keeps an audit trail.
                        let mut payload = frame
                            .get("params")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Default::default()));
                        annotate_protocol_payload(&mut payload, method);
                        let event_type = match method {
                            "item/started" => EventType::ItemStarted,
                            "item/completed" => EventType::ItemCompleted,
                            _ => EventType::ItemUpdated,
                        };
                        let pe =
                            ProviderEvent::new(canonical_turn_id, event_type, "codex", payload);
                        if event_tx.send(pe).await.is_err() {
                            return;
                        }
                    }
                    "turn/started" => {
                        let mut payload = frame
                            .get("params")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Default::default()));
                        annotate_protocol_payload(&mut payload, method);
                        set_payload_field_if_absent(&mut payload, "item_type", "runtime_status");
                        set_payload_field_if_absent(&mut payload, "status", "running");
                        set_payload_field_if_absent(&mut payload, "summary", "Codex turn started");
                        let pe = ProviderEvent::new(
                            canonical_turn_id,
                            EventType::ItemUpdated,
                            "codex",
                            payload,
                        );
                        if event_tx.send(pe).await.is_err() {
                            return;
                        }
                    }
                    "hook/started"
                    | "hook/completed"
                    | "turn/diff/updated"
                    | "turn/plan/updated"
                    | "item/autoApprovalReview/started"
                    | "item/autoApprovalReview/completed"
                    | "item/fileChange/patchUpdated"
                    | "item/fileChange/outputDelta"
                    | "item/plan/delta"
                    | "item/reasoning/summaryTextDelta"
                    | "item/reasoning/summaryPartAdded"
                    | "item/reasoning/textDelta"
                    | "item/mcpToolCall/progress"
                    | "item/commandExecution/terminalInteraction"
                    | "rawResponseItem/completed" => {
                        let mut payload = frame
                            .get("params")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Default::default()));
                        annotate_protocol_payload(&mut payload, method);
                        let event_type = if method.ends_with("/started") || method == "hook/started"
                        {
                            EventType::ItemStarted
                        } else if method.ends_with("/completed") || method == "hook/completed" {
                            EventType::ItemCompleted
                        } else {
                            EventType::ItemUpdated
                        };
                        let pe =
                            ProviderEvent::new(canonical_turn_id, event_type, "codex", payload);
                        if event_tx.send(pe).await.is_err() {
                            return;
                        }
                    }
                    "item/commandExecution/outputDelta"
                    | "command/exec/outputDelta"
                    | "process/outputDelta" => {
                        let params = frame.get("params").unwrap_or(&Value::Null);
                        let decoded_delta = extract_codex_output_delta(params);
                        if stream_debug {
                            debug_codex_output_delta(
                                method,
                                params,
                                decoded_delta.as_deref().map(str::len).unwrap_or(0),
                            );
                        }
                        if let Some(delta) = decoded_delta {
                            let stream = params
                                .get("stream")
                                .and_then(|v| v.as_str())
                                .unwrap_or("merged");
                            if event_tx
                                .send(ProviderEvent::terminal_output(
                                    canonical_turn_id,
                                    "codex",
                                    delta,
                                    Some(stream),
                                    Some("codex_app_server"),
                                ))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }

                        let mut payload = frame
                            .get("params")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Default::default()));
                        annotate_protocol_payload(&mut payload, method);
                        set_payload_field_if_absent(
                            &mut payload,
                            "item_type",
                            "command_output_delta",
                        );
                        let pe = ProviderEvent::new(
                            canonical_turn_id,
                            EventType::ItemUpdated,
                            "codex",
                            payload,
                        );
                        if event_tx.send(pe).await.is_err() {
                            return;
                        }
                    }
                    "process/exited" => {
                        let mut payload = frame
                            .get("params")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Default::default()));
                        annotate_protocol_payload(&mut payload, method);
                        set_payload_field_if_absent(&mut payload, "item_type", "command_execution");
                        set_process_exit_status(&mut payload);
                        let pe = ProviderEvent::new(
                            canonical_turn_id,
                            EventType::ItemUpdated,
                            "codex",
                            payload,
                        );
                        if event_tx.send(pe).await.is_err() {
                            return;
                        }
                    }
                    "turn/completed" => {
                        let mut payload = frame
                            .get("params")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Default::default()));
                        annotate_protocol_payload(&mut payload, method);
                        let _ = event_tx
                            .send(ProviderEvent::new(
                                canonical_turn_id,
                                EventType::TurnCompleted,
                                "codex",
                                payload,
                            ))
                            .await;
                        return;
                    }
                    "turn/failed" => {
                        let err_text = turn_failed_error_text(frame.get("params"));
                        let _ = event_tx
                            .send(ProviderEvent::turn_failed(
                                canonical_turn_id,
                                "codex",
                                err_text,
                            ))
                            .await;
                        return;
                    }
                    // System lifecycle — ignored at the per-turn level.
                    "mcpServer/startupStatus/updated"
                    | "remoteControl/status/changed"
                    | "thread/status/changed"
                    | "thread/started"
                    | "thread/tokenUsage/updated"
                    | "account/rateLimits/updated" => {}
                    _ => {
                        // Unknown notification — surface as raw ItemUpdated for
                        // forward compatibility.
                        let mut payload = frame
                            .get("params")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Default::default()));
                        annotate_protocol_payload(&mut payload, method);
                        let pe = ProviderEvent::new(
                            canonical_turn_id,
                            EventType::ItemUpdated,
                            "codex",
                            payload,
                        );
                        if event_tx.send(pe).await.is_err() {
                            return;
                        }
                    }
                }
            }
            // Stream closed before turn boundary — surface as failure.
            let _ = event_tx
                .send(ProviderEvent::turn_failed(
                    canonical_turn_id,
                    "codex",
                    "codex app-server stdout closed before turn/completed",
                ))
                .await;
        });

        Ok(event_rx)
    }

    async fn update_context(&mut self, _context: ContextBundle) -> Result<(), ProviderError> {
        // No-op: Codex's app-server keeps thread context internally; the
        // caller folds any additional context into the user turn text. A more
        // sophisticated implementation could call a hypothetical `thread/...`
        // method to set per-turn system instructions — none exposed today.
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), ProviderError> {
        // Briefly take the stdin lock to close it cleanly. We don't hold
        // the lock through the grace-period sleep so a concurrent drain
        // task can finish writing any pending approval responses.
        {
            let mut guard = self.stdin.lock().await;
            let _ = guard.shutdown().await;
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            sleep(Duration::from_secs(2)).await;
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill().await;
        }
        Ok(())
    }

    fn is_healthy(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn resume_token(&self) -> Option<String> {
        // The codex `thread.id` is the resume handle the daemon accepts via
        // `thread/resume {threadId}`. Surface it so the GUI / supervisor can
        // persist it in the Switchyard session and reuse on respawn.
        Some(self.thread_id.clone())
    }

    async fn rewind_to(&mut self, turn_index: u32) -> Result<(), ProviderError> {
        // Codex JSON-RPC `thread/fork` rewinds the server-side conversation
        // to immediately before the user message at `turnIndex`, returning
        // a new thread id. We swap the live instance's thread_id atomically
        // so the next `send_message` continues on the forked thread.
        //
        // If the daemon rejects the request (older codex version that
        // doesn't speak `thread/fork`, or an out-of-range turn_index), we
        // surface `UnsupportedCapability` so the caller can fall back to
        // the cold rewind path.
        let req_id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        write_frame_arc(
            &self.stdin,
            json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "method": "thread/fork",
                "params": {
                    "threadId": self.thread_id,
                    "turnIndex": turn_index,
                }
            }),
        )
        .await
        .map_err(|e| ProviderError::ExecutionFailed(format!("thread/fork write: {e}")))?;

        let reply = await_response(&self.frame_rx, req_id).await?;
        if let Some(err) = reply.get("error") {
            return Err(ProviderError::UnsupportedCapability(format!(
                "codex thread/fork rejected: {err}"
            )));
        }
        let new_thread_id = reply
            .get("result")
            .and_then(|r| r.get("thread"))
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ProviderError::ExecutionFailed("missing thread.id in thread/fork reply".into())
            })?
            .to_string();
        self.thread_id = new_thread_id;
        Ok(())
    }
}

fn annotate_protocol_payload(payload: &mut Value, method: &str) {
    let derived_item_type = payload_item_type_from_payload(payload)
        .or_else(|| codex_item_type_from_method(method).map(ToString::to_string));
    if let Value::Object(map) = payload {
        map.entry("method".to_string())
            .or_insert_with(|| Value::String(method.to_string()));
        map.entry("type".to_string())
            .or_insert_with(|| Value::String(method.replace('/', ".")));
        if let Some(item_type) = derived_item_type {
            map.entry("item_type".to_string())
                .or_insert_with(|| Value::String(item_type));
        }
    }
}

fn set_payload_field_if_absent(payload: &mut Value, key: &str, value: &str) {
    if let Value::Object(map) = payload {
        map.entry(key.to_string())
            .or_insert_with(|| Value::String(value.to_string()));
    }
}

fn set_payload_field(payload: &mut Value, key: &str, value: &str) {
    if let Value::Object(map) = payload {
        map.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn set_process_exit_status(payload: &mut Value) {
    if let Some(code) = process_exit_code(payload) {
        let status = if code == 0 { "completed" } else { "failed" };
        set_payload_field(payload, "status", status);
    } else {
        set_payload_field_if_absent(payload, "status", "completed");
    }
}

fn process_exit_code(payload: &Value) -> Option<i64> {
    [
        "exitCode",
        "exit_code",
        "code",
        "exitStatus",
        "exit_status",
        "statusCode",
        "status_code",
    ]
    .into_iter()
    .find_map(|key| json_i64(payload.get(key)))
    .or_else(|| payload.get("exit").and_then(|exit| json_i64(Some(exit))))
    .or_else(|| {
        payload.get("process").and_then(|process| {
            ["exitCode", "exit_code", "code"]
                .into_iter()
                .find_map(|key| json_i64(process.get(key)))
        })
    })
}

fn json_i64(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
}

fn turn_failed_error_text(params: Option<&Value>) -> String {
    params
        .and_then(|params| {
            human_json_text(params.get("error"))
                .or_else(|| human_json_text(params.get("message")))
                .or_else(|| human_json_text(Some(params)).filter(|_| !params.is_object()))
        })
        .unwrap_or_else(|| "turn/failed without error payload".to_string())
}

fn human_json_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some(text.to_string());
    }
    if let Some(message) = value
        .get("message")
        .and_then(|message| message.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some(message.to_string());
    }
    if let Some(error) = value.get("error") {
        return human_json_text(Some(error));
    }
    if !value.is_null() {
        let rendered = value.to_string();
        if !rendered.trim().is_empty() {
            return Some(rendered);
        }
    }
    None
}

fn debug_codex_stream_enabled() -> bool {
    std::env::var("SWITCHYARD_DEBUG_CODEX_STREAM")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn debug_json_keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn debug_codex_stream_notification(
    method: &str,
    params: &Value,
    server_turn_id: Option<&str>,
    canonical_turn_id: Uuid,
) {
    let item_type = payload_item_type_from_payload(params).unwrap_or_else(|| "-".to_string());
    eprintln!(
        "[switchyard codex stream] method={method} canonical_turn_id={canonical_turn_id} server_turn_id={} item_type={item_type} keys={:?}",
        server_turn_id.unwrap_or("-"),
        debug_json_keys(params),
    );
}

fn debug_codex_output_delta(method: &str, params: &Value, decoded_len: usize) {
    let stream = params
        .get("stream")
        .and_then(|value| value.as_str())
        .unwrap_or("merged");
    eprintln!(
        "[switchyard codex outputDelta] method={method} stream={stream} decoded_len={decoded_len} keys={:?}",
        debug_json_keys(params),
    );
}

fn payload_item_type_from_payload(payload: &Value) -> Option<String> {
    [
        payload.get("item_type"),
        payload.get("item").and_then(|item| item.get("type")),
        payload
            .get("params")
            .and_then(|params| params.get("item_type")),
        payload
            .get("params")
            .and_then(|params| params.get("item"))
            .and_then(|item| item.get("type")),
        payload.get("type").filter(|value| {
            value
                .as_str()
                .map(|text| !text.contains('.') && !text.contains('/'))
                .unwrap_or(false)
        }),
    ]
    .into_iter()
    .flatten()
    .filter_map(|value| value.as_str())
    .map(normalize_codex_item_type)
    .find(|item_type| !item_type.is_empty())
}

fn codex_item_type_from_method(method: &str) -> Option<&'static str> {
    match method {
        "turn/started" => Some("runtime_status"),
        "turn/diff/updated" => Some("file_change"),
        "turn/plan/updated" => Some("plan"),
        "hook/started" | "hook/completed" => Some("hook"),
        "rawResponseItem/completed" => Some("raw_response_item"),
        "item/commandExecution/outputDelta" | "command/exec/outputDelta" => {
            Some("command_output_delta")
        }
        "process/outputDelta" => Some("terminal_output"),
        "process/exited" => Some("command_execution"),
        "item/commandExecution/terminalInteraction" => Some("terminal_interaction"),
        "item/fileChange/patchUpdated" => Some("file_change"),
        "item/fileChange/outputDelta" => Some("file_change_delta"),
        "item/plan/delta" => Some("plan"),
        "item/reasoning/summaryTextDelta"
        | "item/reasoning/summaryPartAdded"
        | "item/reasoning/textDelta" => Some("reasoning"),
        "item/mcpToolCall/progress" => Some("mcp_tool_call"),
        "item/autoApprovalReview/started" | "item/autoApprovalReview/completed" => {
            Some("auto_approval_review")
        }
        _ => None,
    }
}

fn normalize_codex_item_type(value: &str) -> String {
    let mut out = String::new();
    let mut prev_was_lower_or_digit = false;
    let mut last_was_separator = false;

    for ch in value.trim().chars() {
        if ch.is_ascii_uppercase() && prev_was_lower_or_digit && !last_was_separator {
            out.push('_');
        }

        if ch == '.' || ch == '/' || ch == '-' || ch == '_' || ch.is_whitespace() {
            if !out.is_empty() && !last_was_separator {
                out.push('_');
                last_was_separator = true;
            }
            prev_was_lower_or_digit = false;
            continue;
        }

        out.push(ch.to_ascii_lowercase());
        last_was_separator = false;
        prev_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
    }

    while out.ends_with('_') {
        out.pop();
    }
    out
}

fn extract_codex_output_delta(params: &Value) -> Option<String> {
    params
        .get("delta")
        .and_then(|delta| {
            non_empty_string(delta)
                .or_else(|| delta.get("text").and_then(non_empty_string))
                .or_else(|| delta.get("content").and_then(content_text))
                .or_else(|| delta.get("deltaBase64").and_then(decode_base64_text))
                .or_else(|| delta.get("delta_base64").and_then(decode_base64_text))
                .or_else(|| {
                    delta
                        .get("delta")
                        .and_then(|inner| extract_codex_output_delta(&json!({ "delta": inner })))
                })
        })
        .or_else(|| params.get("deltaBase64").and_then(decode_base64_text))
        .or_else(|| params.get("delta_base64").and_then(decode_base64_text))
        .or_else(|| params.get("text").and_then(non_empty_string))
        .or_else(|| params.get("output").and_then(non_empty_string))
}

fn decode_base64_text(value: &Value) -> Option<String> {
    let encoded = value.as_str()?.trim();
    if encoded.is_empty() {
        return None;
    }
    let bytes = STANDARD.decode(encoded).ok()?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    (!text.is_empty()).then_some(text)
}

fn is_text_delta_method(method: &str) -> bool {
    let normalized = method.to_ascii_lowercase().replace(['/', '_', '-'], ".");
    normalized.contains("agentmessage.delta")
        || normalized.contains("agent.message.delta")
        || normalized.contains("agent.message")
        || normalized.contains("agent_message")
        || normalized.contains("assistant.message.delta")
        || normalized == "item.delta"
        || normalized.ends_with(".text.delta")
        || normalized.ends_with(".message.delta")
        || normalized.ends_with(".content.delta")
}

fn is_textish_delta_kind(kind: Option<&str>) -> bool {
    let Some(kind) = kind else {
        return true;
    };
    let normalized = kind.to_ascii_lowercase().replace(['/', '_', '-'], ".");
    normalized.contains("agent.message")
        || normalized.contains("assistant")
        || normalized.contains("message.delta")
        || normalized.contains("content.block.delta")
        || normalized.contains("text.delta")
        || normalized == "text"
        || normalized == "output.text"
        || normalized == "agent.message.delta"
}

fn extract_codex_text_delta(method: &str, params: &Value) -> Option<String> {
    let method_hint = method.to_ascii_lowercase();
    if let Some(text) = params.get("delta").and_then(|delta| {
        extract_codex_delta_text(
            delta,
            method_hint.contains("agentmessage")
                || method_hint.contains("agent_message")
                || method_hint.contains("assistant")
                || method_hint.contains("message")
                || method_hint.contains("content"),
        )
    }) {
        return Some(text);
    }

    let item = params.get("item");
    let item_type = item
        .and_then(|item| item.get("type"))
        .and_then(|value| value.as_str())
        .map(normalize_codex_item_type);
    if matches!(
        item_type.as_deref(),
        Some("agent_message" | "assistant" | "message")
    ) {
        if let Some(text) = item
            .and_then(|item| item.get("text"))
            .and_then(non_empty_string)
        {
            return Some(text);
        }
        if let Some(text) = item
            .and_then(|item| item.get("content"))
            .and_then(content_text)
        {
            return Some(text);
        }
    }

    let role = params
        .get("message")
        .and_then(|message| message.get("role"))
        .or_else(|| params.get("role"))
        .and_then(|value| value.as_str());
    if role == Some("assistant")
        && let Some(text) = params
            .get("message")
            .and_then(|message| message.get("content"))
            .or_else(|| params.get("content"))
            .and_then(content_text)
    {
        return Some(text);
    }

    None
}

fn extract_codex_delta_text(delta: &Value, method_has_text_hint: bool) -> Option<String> {
    if let Some(text) = non_empty_string(delta) {
        return method_has_text_hint.then_some(text);
    }

    let kind = delta.get("type").and_then(|value| value.as_str());
    let kind_is_textish = kind
        .map(|kind| is_textish_delta_kind(Some(kind)))
        .unwrap_or(false);
    if kind.is_some() && !kind_is_textish {
        return None;
    }
    if !method_has_text_hint && !kind_is_textish {
        return None;
    }

    let nested_text_hint = method_has_text_hint || kind_is_textish;
    if let Some(text) = delta.get("text").and_then(non_empty_string) {
        return Some(text);
    }
    if let Some(text) = delta.get("content").and_then(content_text) {
        return Some(text);
    }
    if let Some(text) = delta
        .get("delta")
        .and_then(|inner| extract_codex_delta_text(inner, nested_text_hint))
    {
        return Some(text);
    }
    if let Some(text) = delta
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(content_text)
    {
        return Some(text);
    }
    None
}

fn content_text(value: &Value) -> Option<String> {
    if let Some(text) = non_empty_string(value) {
        return Some(text);
    }

    if let Some(blocks) = value.as_array() {
        let mut joined = String::new();
        for block in blocks {
            if let Some(text) = non_empty_string(block) {
                joined.push_str(&text);
                continue;
            }
            if let Some(text) = block.get("text").and_then(non_empty_string) {
                joined.push_str(&text);
                continue;
            }
            if let Some(text) = block.get("content").and_then(content_text) {
                joined.push_str(&text);
            }
        }
        return (!joined.is_empty()).then_some(joined);
    }

    if value.is_object() {
        if let Some(text) = value.get("text").and_then(non_empty_string) {
            return Some(text);
        }
        if let Some(text) = value.get("content").and_then(content_text) {
            return Some(text);
        }
    }

    None
}

fn non_empty_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn response_turn_id(frame: &Value) -> Option<&str> {
    frame
        .get("result")
        .and_then(|r| r.get("turn"))
        .and_then(|t| t.get("id"))
        .and_then(|v| v.as_str())
}

fn notification_turn_id(frame: &Value) -> Option<&str> {
    frame
        .get("params")
        .and_then(|p| p.get("turnId"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            frame
                .get("params")
                .and_then(|p| p.get("turn"))
                .and_then(|t| t.get("id"))
                .and_then(|v| v.as_str())
        })
}

fn should_bind_server_turn_id(method: &str) -> bool {
    method == "turn/started"
        || method == "turn/completed"
        || method == "turn/failed"
        || method.starts_with("item/")
        || method.starts_with("hook/")
        || method.starts_with("turn/diff")
        || method.starts_with("turn/plan")
        || method == "rawResponseItem/completed"
}

fn is_approval_method(method: &str) -> bool {
    method.contains("requestApproval") || method.contains("approval")
}

fn is_legacy_permissive_policy(policy: &ExecutionPolicy) -> bool {
    policy.write_access
        && policy.allowed_paths.is_empty()
        && lexical_normalize(&policy.cwd) == Path::new(".")
}

fn approval_request_id(turn_id: Uuid, rpc_id: &Value, method: &str) -> String {
    let method_slug: String = method
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect();
    format!("codex:{turn_id}:{method_slug}:{}", rpc_id_component(rpc_id))
}

fn rpc_id_component(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => "null".to_string(),
        _ => value.to_string(),
    }
}

fn unix_epoch_millis() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration
            .as_secs()
            .saturating_mul(1_000)
            .saturating_add(u64::from(duration.subsec_millis())),
        Err(_) => 0,
    }
}

fn sandbox_mode_name(mode: EffectiveSandboxMode) -> &'static str {
    match mode {
        EffectiveSandboxMode::ReadOnly => "read-only",
        EffectiveSandboxMode::WorkspaceWrite => "workspace-write",
        EffectiveSandboxMode::DangerFullAccess => "danger-full-access",
    }
}

fn approval_policy_payload(policy: &ExecutionPolicy) -> Value {
    json!({
        "sandbox_mode": sandbox_mode_name(policy.effective_sandbox_mode()),
        "write_access": policy.write_access,
        "cwd": policy.cwd.display().to_string(),
        "allowed_paths": policy.allowed_paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
    })
}

fn turn_input_items(input: &TurnInput) -> Vec<Value> {
    let mut input_items = vec![json!({
        "type": "text",
        "text": input.user_message_text(),
        "text_elements": [],
    })];
    for attachment in &input.attachments {
        if matches!(attachment.mime_type.as_deref(), Some(mime) if mime.starts_with("image/")) {
            input_items.push(json!({
                "type": "localImage",
                "path": attachment.path.display().to_string(),
            }));
        }
    }
    input_items
}

fn turn_start_params(thread_id: &str, input_items: Vec<Value>, policy: &ExecutionPolicy) -> Value {
    json!({
        "threadId": thread_id,
        "input": input_items,
        // Keep Codex's daemon-side working root, approval posture, and
        // filesystem sandbox aligned with Switchyard's per-turn GUI choice.
        // Without these overrides the long-lived app-server thread can keep
        // using stale defaults even after the user switches the quick
        // permission mode in the composer.
        "cwd": codex_cwd_payload(policy),
        "approvalPolicy": codex_approval_policy_payload(policy),
        "approvalsReviewer": "user",
        "sandboxPolicy": codex_sandbox_policy_payload(policy),
    })
}

fn codex_approval_policy_payload(policy: &ExecutionPolicy) -> Value {
    match policy.effective_sandbox_mode() {
        // Full access means there is no Codex-side sandbox escape to review.
        // Switchyard still handles any explicit approval RPCs defensively, but
        // the daemon should not auto-route them to guardian/default rejection.
        EffectiveSandboxMode::DangerFullAccess => json!("never"),
        // For sandboxed modes the model may request permission escalation; the
        // request is routed to this JSON-RPC client (`approvalsReviewer=user`)
        // and then surfaced as an `approval_request` session-stream item.
        EffectiveSandboxMode::ReadOnly | EffectiveSandboxMode::WorkspaceWrite => {
            json!("on-request")
        }
    }
}

fn codex_sandbox_policy_payload(policy: &ExecutionPolicy) -> Value {
    match policy.effective_sandbox_mode() {
        EffectiveSandboxMode::DangerFullAccess => json!({
            "type": "dangerFullAccess",
        }),
        EffectiveSandboxMode::ReadOnly => json!({
            "type": "readOnly",
            // Switchyard currently exposes a filesystem sandbox toggle, not a
            // network toggle. Preserve the process/network posture instead of
            // introducing a hidden network-deny side effect when the user only
            // changes the file permission mode.
            "networkAccess": true,
        }),
        EffectiveSandboxMode::WorkspaceWrite => {
            let sandbox = json!({
                "type": "workspaceWrite",
                "writableRoots": codex_writable_roots_payload(policy),
                "networkAccess": true,
            });
            // These tmpdir flags are Unix-oriented. Keep them out of the
            // Windows app-server payload so Codex's Windows sandbox
            // initializer only receives platform-relevant fields.
            #[cfg(not(windows))]
            {
                let mut sandbox = sandbox;
                if let Some(obj) = sandbox.as_object_mut() {
                    obj.insert("excludeTmpdirEnvVar".to_string(), json!(false));
                    obj.insert("excludeSlashTmp".to_string(), json!(false));
                }
                sandbox
            }
            #[cfg(windows)]
            {
                sandbox
            }
        }
    }
}

fn codex_cwd_payload(policy: &ExecutionPolicy) -> String {
    codex_path_payload(&policy.cwd, Path::new("."))
}

fn codex_writable_roots_payload(policy: &ExecutionPolicy) -> Vec<String> {
    let source_paths = if policy.allowed_paths.is_empty() {
        vec![policy.cwd.clone()]
    } else {
        policy.allowed_paths.clone()
    };

    let mut roots = Vec::new();
    for path in source_paths {
        let rendered = codex_path_payload(&path, &policy.cwd);
        if !roots.iter().any(|existing| existing == &rendered) {
            roots.push(rendered);
        }
    }

    if roots.is_empty() {
        roots.push(codex_cwd_payload(policy));
    }
    roots
}

fn codex_path_payload(path: &Path, cwd: &Path) -> String {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let base = if cwd.is_absolute() {
            cwd.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|current| current.join(cwd))
                .unwrap_or_else(|_| PathBuf::from(".").join(cwd))
        };
        base.join(path)
    };
    lexical_normalize(&resolved).display().to_string()
}

/// Compute the JSON-RPC response payload + audit tag for a Codex
/// server-initiated request, gated by an [`ExecutionPolicy`].
///
/// Returns `(result, tag)` where `result` is what we hand back as the
/// JSON-RPC `result` field, and `tag` is a stable short string for telemetry
/// / UI rendering ("approve:permissive", "deny:no_write_access",
/// "deny:outside_allowed_paths", …).
///
/// Decision matrix:
/// - Non-approval methods → `{}` (soft no-op). Codex treats this as ack.
/// - Approval methods with `policy.write_access == false` → deny.
/// - File-change approvals where the target path is outside `policy.cwd`
///   AND not under any entry in `policy.allowed_paths` → deny.
/// - Otherwise approve.
fn approval_decision(
    method: &str,
    params: &Value,
    policy: &ExecutionPolicy,
) -> (Value, &'static str) {
    if !is_approval_method(method) {
        return (json!({}), "noop");
    }

    if !policy.write_access {
        return (json!({ "decision": "deny" }), "deny:no_write_access");
    }

    // Empty allowed_paths + write_access=true is the `permissive()` preset:
    // skip path gating entirely. Tag it as permissive so audit logs make
    // the difference visible.
    let permissive_preset = policy.allowed_paths.is_empty() && policy.write_access;
    if permissive_preset {
        return (json!({ "decision": "approve" }), "approve:permissive");
    }

    // Path-aware gating only for fileChange approvals; commandExecution
    // approvals are gated by write_access alone (we'd need to parse the
    // shell command to know what files it touches, which is out of scope).
    if method.contains("fileChange") {
        let target = extract_approval_target_path(params);
        if let Some(path) = target
            && !is_path_allowed(&path, &policy.allowed_paths, &policy.cwd)
        {
            return (json!({ "decision": "deny" }), "deny:outside_allowed_paths");
        }
    }

    (json!({ "decision": "approve" }), "approve:within_policy")
}

/// Pull the file path out of a `fileChange/requestApproval` params object.
/// Codex's payload shape isn't formally fixed across versions; we look at a
/// few likely fields and return the first absolute-ish path we find.
fn extract_approval_target_path(params: &Value) -> Option<PathBuf> {
    for key in &["path", "filePath", "file", "target"] {
        if let Some(p) = params.get(*key).and_then(|v| v.as_str()) {
            return Some(PathBuf::from(p));
        }
    }
    // Some payloads nest under `change.path` etc.
    if let Some(change) = params.get("change") {
        for key in &["path", "filePath", "file", "target"] {
            if let Some(p) = change.get(*key).and_then(|v| v.as_str()) {
                return Some(PathBuf::from(p));
            }
        }
    }
    None
}

/// `target` is "allowed" iff it's inside `cwd` OR inside any of the
/// `allowed_paths`. Empty `allowed_paths` means "no extra paths beyond
/// cwd". Relative paths are resolved against `cwd` before comparison.
fn is_path_allowed(target: &Path, allowed: &[PathBuf], cwd: &Path) -> bool {
    let cwd = lexical_normalize(cwd);
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        cwd.join(target)
    };
    let resolved = lexical_normalize(&resolved);

    if resolved.starts_with(&cwd) {
        return true;
    }
    allowed
        .iter()
        .map(|path| lexical_normalize(path))
        .any(|path| resolved.starts_with(path))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(component.as_os_str()),
            },
            _ => out.push(component.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// Run a `thread/start` handshake on an initialized daemon and return the
/// minted thread id. Factored out so the resume path can fall back to it
/// when the daemon refuses the resume token.
async fn fresh_thread_start(
    stdin: &Arc<Mutex<ChildStdin>>,
    frame_rx: &Arc<Mutex<mpsc::Receiver<Value>>>,
    next_id: &Arc<AtomicI64>,
) -> Result<String, ProviderError> {
    let ts_id = next_id.fetch_add(1, Ordering::SeqCst) + 1;
    write_frame_arc(
        stdin,
        json!({
            "jsonrpc": "2.0",
            "id": ts_id,
            "method": "thread/start",
            "params": {}
        }),
    )
    .await
    .map_err(|e| ProviderError::ExecutionFailed(format!("thread/start write: {e}")))?;

    let thread_reply = await_response(frame_rx, ts_id).await?;
    thread_reply
        .get("result")
        .and_then(|r| r.get("thread"))
        .and_then(|t| t.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            ProviderError::ExecutionFailed("missing thread.id in thread/start reply".into())
        })
}

async fn write_frame_arc(stdin: &Arc<Mutex<ChildStdin>>, frame: Value) -> std::io::Result<()> {
    let mut s = frame.to_string();
    s.push('\n');
    let mut guard = stdin.lock().await;
    guard.write_all(s.as_bytes()).await?;
    guard.flush().await
}

async fn await_response(
    rx_lock: &Arc<Mutex<mpsc::Receiver<Value>>>,
    id: i64,
) -> Result<Value, ProviderError> {
    let id_value = Value::from(id);
    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
    let mut rx = rx_lock.lock().await;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = timeout(remaining, rx.recv())
            .await
            .map_err(|_| {
                ProviderError::ExecutionFailed(format!("timeout waiting for response id={id}"))
            })?
            .ok_or_else(|| {
                ProviderError::ExecutionFailed(format!("stdout closed waiting for id={id}"))
            })?;
        if frame.get("id") == Some(&id_value) {
            return Ok(frame);
        }
        // Notification or response for a different id — discard during setup.
        // The post-setup `send_message` drain task takes ownership via its
        // own lock and processes notifications normally; this discard window
        // is bounded to the synchronous initialize/thread-start handshake.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    async fn clear_pending_approvals_for_test() {
        approval_registry().lock().await.clear();
    }

    #[test]
    fn annotate_protocol_payload_adds_method_and_normalized_type() {
        let mut payload =
            json!({ "turnId": "server-turn", "item": { "type": "command_execution" } });

        annotate_protocol_payload(&mut payload, "item/started");

        assert_eq!(
            payload.get("method").and_then(|v| v.as_str()),
            Some("item/started")
        );
        assert_eq!(
            payload.get("type").and_then(|v| v.as_str()),
            Some("item.started")
        );
        assert_eq!(
            payload.get("item_type").and_then(|v| v.as_str()),
            Some("command_execution")
        );
    }

    #[test]
    fn annotate_protocol_payload_normalizes_camel_case_item_type() {
        let mut payload =
            json!({ "turnId": "server-turn", "item": { "type": "commandExecution" } });

        annotate_protocol_payload(&mut payload, "item/started");

        assert_eq!(
            payload.get("item_type").and_then(|v| v.as_str()),
            Some("command_execution")
        );
    }

    #[test]
    fn annotate_protocol_payload_maps_codex_stream_methods() {
        let mut payload = json!({ "turnId": "server-turn", "itemId": "item-1", "delta": "stdout" });

        annotate_protocol_payload(&mut payload, "item/commandExecution/outputDelta");

        assert_eq!(
            payload.get("item_type").and_then(|v| v.as_str()),
            Some("command_output_delta")
        );
    }

    #[test]
    fn process_exited_status_uses_exit_code() {
        let mut success = json!({ "exitCode": 0 });
        set_process_exit_status(&mut success);
        assert_eq!(
            success.get("status").and_then(|v| v.as_str()),
            Some("completed")
        );

        let mut failure = json!({ "exit_code": "2" });
        set_process_exit_status(&mut failure);
        assert_eq!(
            failure.get("status").and_then(|v| v.as_str()),
            Some("failed")
        );
    }

    #[test]
    fn turn_failed_error_text_avoids_json_string_quotes() {
        assert_eq!(
            turn_failed_error_text(Some(&json!({ "error": "plain failure" }))),
            "plain failure"
        );
        assert_eq!(
            turn_failed_error_text(Some(&json!({ "error": { "message": "nested failure" } }))),
            "nested failure"
        );
    }

    #[test]
    fn extract_codex_output_delta_decodes_delta_base64() {
        let payload = json!({ "deltaBase64": "aGVsbG8K" });

        assert_eq!(
            extract_codex_output_delta(&payload).as_deref(),
            Some("hello\n")
        );
    }

    #[test]
    fn extract_codex_output_delta_decodes_nested_delta_base64() {
        let payload = json!({ "delta": { "deltaBase64": "c3Rkb3V0" } });

        assert_eq!(
            extract_codex_output_delta(&payload).as_deref(),
            Some("stdout")
        );
    }

    #[test]
    fn annotate_protocol_payload_preserves_existing_type() {
        let mut payload = json!({ "type": "custom.type" });

        annotate_protocol_payload(&mut payload, "item/completed");

        assert_eq!(
            payload.get("method").and_then(|v| v.as_str()),
            Some("item/completed")
        );
        assert_eq!(
            payload.get("type").and_then(|v| v.as_str()),
            Some("custom.type")
        );
    }

    #[test]
    fn text_delta_detection_handles_codex_shapes() {
        assert!(is_text_delta_method("item/agentMessage/delta"));
        assert!(is_text_delta_method("item.delta"));
        assert!(is_text_delta_method("response/output_text/delta"));

        let params = json!({
            "delta": { "type": "agent_message_delta", "text": "hello" }
        });
        assert_eq!(
            extract_codex_text_delta("item.delta", &params).as_deref(),
            Some("hello")
        );

        let string_delta = json!({ "delta": " world" });
        assert_eq!(
            extract_codex_text_delta("item/agentMessage/delta", &string_delta).as_deref(),
            Some(" world")
        );

        let content_blocks = json!({
            "item": {
                "type": "agent_message",
                "content": [
                    { "type": "text", "text": "foo" },
                    { "type": "text", "text": "bar" }
                ]
            }
        });
        assert_eq!(
            extract_codex_text_delta("item/updated", &content_blocks).as_deref(),
            Some("foobar")
        );

        let camel_case_item = json!({
            "item": {
                "type": "agentMessage",
                "text": "camel"
            }
        });
        assert_eq!(
            extract_codex_text_delta("item/completed", &camel_case_item).as_deref(),
            Some("camel")
        );
    }

    #[test]
    fn non_text_delta_is_not_extracted_without_method_hint() {
        let params = json!({
            "delta": { "type": "tool_output_delta", "text": "stdout" }
        });

        assert_eq!(extract_codex_text_delta("item.delta", &params), None);

        let string_delta = json!({
            "delta": "PWD:\nE:\\Switchyard\nROOT:\n..."
        });

        assert_eq!(extract_codex_text_delta("item.delta", &string_delta), None);
    }

    #[test]
    fn non_text_delta_is_not_extracted_with_content_method_hint() {
        let params = json!({
            "delta": { "type": "tool_output_delta", "text": "stdout" }
        });

        assert_eq!(
            extract_codex_text_delta("response/content/delta", &params),
            None
        );
    }

    #[test]
    fn response_turn_id_reads_turn_start_reply() {
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": { "turn": { "id": "turn-123" } }
        });

        assert_eq!(response_turn_id(&frame), Some("turn-123"));
    }

    #[test]
    fn notification_turn_id_reads_flat_and_nested_shapes() {
        assert_eq!(
            notification_turn_id(&json!({
                "method": "item/started",
                "params": { "turnId": "flat-turn" }
            })),
            Some("flat-turn")
        );
        assert_eq!(
            notification_turn_id(&json!({
                "method": "turn/started",
                "params": { "turn": { "id": "nested-turn" } }
            })),
            Some("nested-turn")
        );
    }

    #[test]
    fn server_turn_id_binding_is_limited_to_turn_scoped_notifications() {
        assert!(should_bind_server_turn_id("turn/started"));
        assert!(should_bind_server_turn_id(
            "item/commandExecution/outputDelta"
        ));
        assert!(should_bind_server_turn_id("hook/started"));
        assert!(!should_bind_server_turn_id("thread/status/changed"));
        assert!(!should_bind_server_turn_id("process/outputDelta"));
    }

    #[tokio::test]
    async fn gui_approval_approve_resolves_to_codex_approve() {
        let _guard = approval_test_lock().lock().await;
        clear_pending_approvals_for_test().await;

        let request_id = "approval-test-approve";
        let approval_rx = register_pending_approval(request_id.to_string()).await;
        submit_tool_approval_decision(
            request_id,
            ToolApprovalDecision::Approve,
            Some("accepted from test".to_string()),
        )
        .await
        .expect("submit approval");

        let resolution = wait_for_gui_approval(request_id, approval_rx).await;
        assert_eq!(
            resolution.decision.get("decision").and_then(|v| v.as_str()),
            Some("approve")
        );
        assert_eq!(resolution.audit_tag, "approve:user");
        assert_eq!(resolution.status, "completed");
        assert_eq!(resolution.reason.as_deref(), Some("accepted from test"));

        clear_pending_approvals_for_test().await;
    }

    #[tokio::test]
    async fn unknown_approval_request_reports_pending_snapshot() {
        let _guard = approval_test_lock().lock().await;
        clear_pending_approvals_for_test().await;

        let _rx = register_pending_approval("pending-sample".to_string()).await;
        let err =
            submit_tool_approval_decision("missing-request", ToolApprovalDecision::Approve, None)
                .await
                .expect_err("missing approval should fail");

        assert!(err.contains("approval request 'missing-request' not found"));
        assert!(err.contains("pending=1"));
        assert!(err.contains("pending-sample"));

        clear_pending_approvals_for_test().await;
    }

    #[test]
    fn turn_start_params_carries_cwd_approval_reviewer_and_sandbox_policy() {
        let repo = std::env::current_dir()
            .expect("current dir")
            .join("target/switchyard-test/repo");
        let shared = std::env::current_dir()
            .expect("current dir")
            .join("target/switchyard-test/shared");
        let policy =
            ExecutionPolicy::workspace_write(repo.clone()).add_allowed_paths([shared.clone()]);
        let params = turn_start_params(
            "thread-1",
            vec![json!({"type": "text", "text": "hello", "text_elements": []})],
            &policy,
        );
        let expected_repo = codex_path_payload(&repo, Path::new("."));
        let expected_shared = codex_path_payload(&shared, &repo);

        assert_eq!(
            params.get("threadId").and_then(|v| v.as_str()),
            Some("thread-1")
        );
        assert_eq!(
            params.get("cwd").and_then(|v| v.as_str()),
            Some(expected_repo.as_str())
        );
        assert_eq!(
            params.get("approvalPolicy").and_then(|v| v.as_str()),
            Some("on-request")
        );
        assert_eq!(
            params.get("approvalsReviewer").and_then(|v| v.as_str()),
            Some("user")
        );
        let sandbox = params.get("sandboxPolicy").expect("sandbox policy");
        assert_eq!(
            sandbox.get("type").and_then(|v| v.as_str()),
            Some("workspaceWrite")
        );
        assert_eq!(
            sandbox.get("networkAccess").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            sandbox
                .get("writableRoots")
                .and_then(|v| v.as_array())
                .map(|roots| roots
                    .iter()
                    .filter_map(|root| root.as_str())
                    .collect::<Vec<_>>()),
            Some(vec![expected_repo.as_str(), expected_shared.as_str()])
        );
        #[cfg(windows)]
        {
            assert!(sandbox.get("excludeTmpdirEnvVar").is_none());
            assert!(sandbox.get("excludeSlashTmp").is_none());
        }
        #[cfg(not(windows))]
        {
            assert_eq!(
                sandbox.get("excludeTmpdirEnvVar").and_then(|v| v.as_bool()),
                Some(false)
            );
            assert_eq!(
                sandbox.get("excludeSlashTmp").and_then(|v| v.as_bool()),
                Some(false)
            );
        }
    }

    #[test]
    fn turn_input_items_keep_image_structured_without_prompt_boilerplate() {
        let image_path = PathBuf::from(
            r"C:\Users\demo\.switchyard\clipboard_attachments\20260523T164228233Z_image.png",
        );
        let input = TurnInput::text("图片输入测试").with_attachments(vec![
            switchyard_provider_api::InputAttachment {
                path: image_path.clone(),
                mime_type: Some("image/png".to_string()),
            },
        ]);

        let items = turn_input_items(&input);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].get("type").and_then(|v| v.as_str()), Some("text"));
        assert_eq!(
            items[0].get("text").and_then(|v| v.as_str()),
            Some("图片输入测试")
        );
        let text = items[0].get("text").and_then(|v| v.as_str()).unwrap();
        assert!(!text.contains("[Switchyard Attachments]"));
        assert!(!text.contains("clipboard_attachments"));
        assert!(!text.contains("20260523T164228233Z_image.png"));

        assert_eq!(
            items[1].get("type").and_then(|v| v.as_str()),
            Some("localImage")
        );
        let expected_path = image_path.display().to_string();
        assert_eq!(
            items[1].get("path").and_then(|v| v.as_str()),
            Some(expected_path.as_str())
        );
    }

    #[test]
    fn codex_sandbox_policy_payload_maps_read_only_and_danger_modes() {
        let read_only = codex_sandbox_policy_payload(&ExecutionPolicy::read_only("/repo"));
        assert_eq!(
            read_only.get("type").and_then(|v| v.as_str()),
            Some("readOnly")
        );
        assert_eq!(
            read_only.get("networkAccess").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            codex_approval_policy_payload(&ExecutionPolicy::read_only("/repo")).as_str(),
            Some("on-request")
        );

        let danger = codex_sandbox_policy_payload(&ExecutionPolicy::danger_full_access("/repo"));
        assert_eq!(
            danger.get("type").and_then(|v| v.as_str()),
            Some("dangerFullAccess")
        );
        assert_eq!(
            codex_approval_policy_payload(&ExecutionPolicy::danger_full_access("/repo")).as_str(),
            Some("never")
        );
    }

    #[test]
    fn permissive_policy_approves_command_execution() {
        let (d, tag) = approval_decision(
            "item/commandExecution/requestApproval",
            &json!({}),
            &ExecutionPolicy::permissive(),
        );
        assert_eq!(d.get("decision").and_then(|v| v.as_str()), Some("approve"));
        assert_eq!(tag, "approve:permissive");
    }

    #[test]
    fn permissive_policy_approves_file_change() {
        let (d, tag) = approval_decision(
            "item/fileChange/requestApproval",
            &json!({"path": "/anywhere/else.txt"}),
            &ExecutionPolicy::permissive(),
        );
        assert_eq!(d.get("decision").and_then(|v| v.as_str()), Some("approve"));
        assert_eq!(tag, "approve:permissive");
    }

    #[test]
    fn unknown_method_returns_empty_soft_noop() {
        let (d, tag) = approval_decision(
            "item/somethingUnrelated",
            &json!({}),
            &ExecutionPolicy::permissive(),
        );
        assert!(d.as_object().map(|o| o.is_empty()).unwrap_or(false));
        assert_eq!(tag, "noop");
    }

    #[test]
    fn default_policy_denies_writes() {
        // `Default` policy is conservative: write_access=false denies any
        // approval-style request that gets this far.
        let (d, tag) = approval_decision(
            "item/commandExecution/requestApproval",
            &json!({}),
            &ExecutionPolicy::default(),
        );
        assert_eq!(d.get("decision").and_then(|v| v.as_str()), Some("deny"));
        assert_eq!(tag, "deny:no_write_access");
    }

    #[test]
    fn file_change_outside_allowed_paths_is_denied() {
        let policy = ExecutionPolicy {
            timeout_secs: 0,
            write_access: true,
            cwd: PathBuf::from("/project"),
            allowed_paths: vec![PathBuf::from("/project/src")],
        };
        let (d, tag) = approval_decision(
            "item/fileChange/requestApproval",
            &json!({"path": "/etc/passwd"}),
            &policy,
        );
        assert_eq!(d.get("decision").and_then(|v| v.as_str()), Some("deny"));
        assert_eq!(tag, "deny:outside_allowed_paths");
    }

    #[test]
    fn file_change_inside_cwd_is_approved_within_policy() {
        let policy = ExecutionPolicy {
            timeout_secs: 0,
            write_access: true,
            cwd: PathBuf::from("/project"),
            allowed_paths: vec![PathBuf::from("/tmp/scratch")],
        };
        let (d, tag) = approval_decision(
            "item/fileChange/requestApproval",
            &json!({"path": "/project/src/main.rs"}),
            &policy,
        );
        assert_eq!(d.get("decision").and_then(|v| v.as_str()), Some("approve"));
        assert_eq!(tag, "approve:within_policy");
    }

    #[test]
    fn file_change_in_extra_allowed_path_is_approved() {
        let policy = ExecutionPolicy {
            timeout_secs: 0,
            write_access: true,
            cwd: PathBuf::from("/project"),
            allowed_paths: vec![PathBuf::from("/tmp/scratch")],
        };
        let (d, tag) = approval_decision(
            "item/fileChange/requestApproval",
            &json!({"path": "/tmp/scratch/output.log"}),
            &policy,
        );
        assert_eq!(d.get("decision").and_then(|v| v.as_str()), Some("approve"));
        assert_eq!(tag, "approve:within_policy");
    }

    #[test]
    fn path_gating_denies_relative_parent_escape() {
        assert!(!is_path_allowed(
            Path::new("../outside.txt"),
            &[PathBuf::from("/project")],
            Path::new("/project")
        ));
    }

    #[test]
    fn path_gating_denies_absolute_parent_escape() {
        assert!(!is_path_allowed(
            Path::new("/project/../etc/passwd"),
            &[PathBuf::from("/project")],
            Path::new("/project")
        ));
    }

    #[test]
    fn path_gating_does_not_allow_sibling_prefix_collision() {
        assert!(!is_path_allowed(
            Path::new("/project2/file.txt"),
            &[PathBuf::from("/project")],
            Path::new("/project")
        ));
    }

    #[test]
    fn path_gating_allows_extra_allowed_path_after_normalization() {
        assert!(is_path_allowed(
            Path::new("/shared/cache/out.txt"),
            &[PathBuf::from("/tmp/../shared")],
            Path::new("/project")
        ));
    }

    #[test]
    fn extract_approval_target_path_handles_nested_change() {
        let p = extract_approval_target_path(&json!({
            "change": { "filePath": "/foo/bar.rs" }
        }));
        assert_eq!(p, Some(PathBuf::from("/foo/bar.rs")));
    }
}
