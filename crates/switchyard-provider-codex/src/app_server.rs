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
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use switchyard_provider_api::{
    ContextBundle, EventType, ExecutionPolicy, LiveInstance, ProviderError, ProviderEvent,
    TurnInput,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
        let mut input_items = vec![json!({
            "type": "text",
            "text": input.user_message_with_attachment_references(),
        })];
        for attachment in &input.attachments {
            input_items.push(json!({
                "type": "localImage",
                "path": attachment.path.display().to_string(),
            }));
        }
        // Send turn/start; response carries the server-side turn id which we
        // use to filter incoming notifications.
        let req_id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        write_frame_arc(
            &self.stdin,
            json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "method": "turn/start",
                "params": {
                    "threadId": self.thread_id,
                    "input": input_items,
                }
            }),
        )
        .await
        .map_err(|e| ProviderError::ExecutionFailed(format!("turn/start write: {e}")))?;

        let turn_reply = await_response(&self.frame_rx, req_id).await?;
        let server_turn_id = turn_reply
            .get("result")
            .and_then(|r| r.get("turn"))
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ProviderError::ExecutionFailed("missing turn.id in turn/start reply".into())
            })?
            .to_string();

        let (event_tx, event_rx) = mpsc::channel::<ProviderEvent>(256);
        let rx_lock = Arc::clone(&self.frame_rx);
        // Approval responses (Slice H) need to land on the same stdin the
        // turn/start invocation used. We share the lock — the per-frame write
        // is sub-millisecond so contention with the caller is negligible.
        let stdin_lock = Arc::clone(&self.stdin);
        // Switchyard's view: each call gets a fresh logical turn_id, distinct
        // from the codex-server-side turn id we filter by.
        let canonical_turn_id = Uuid::now_v7();
        let server_turn_id_clone = server_turn_id;

        tokio::spawn(async move {
            let mut rx = rx_lock.lock().await;
            while let Some(frame) = rx.recv().await {
                // Server-initiated request — codex daemon asking us to
                // approve a tool/file action. Decision routes through the
                // policy: permissive (default for `send_message`) always
                // approves; user-facing flows pass a real policy that gates
                // writes outside `allowed_paths` or when `write_access`
                // is false. The decision is surfaced as an ItemUpdated so
                // the diagnostics drawer / chat ticker can render what
                // happened.
                if let (Some(req_id_val), Some(method_val)) = (
                    frame.get("id").cloned(),
                    frame.get("method").and_then(|m| m.as_str()),
                ) {
                    let params = frame
                        .get("params")
                        .cloned()
                        .unwrap_or_else(|| Value::Object(Default::default()));

                    let (decision, audit_tag) = approval_decision(method_val, &params, &policy);

                    let _ = event_tx
                        .send(ProviderEvent::new(
                            canonical_turn_id,
                            EventType::ItemUpdated,
                            "codex",
                            json!({
                                "item_type": "approval_decision",
                                "method": method_val,
                                "decision_tag": audit_tag,
                                "decision": decision.clone(),
                                "request": params,
                            }),
                        ))
                        .await;

                    let reply = json!({
                        "jsonrpc": "2.0",
                        "id": req_id_val,
                        "result": decision,
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

                let method = match frame.get("method").and_then(|m| m.as_str()) {
                    Some(m) => m,
                    None => continue, // unmatched response
                };

                // Filter to this turn (when params carry turnId).
                if let Some(tid) = frame
                    .get("params")
                    .and_then(|p| p.get("turnId"))
                    .and_then(|v| v.as_str())
                    && tid != server_turn_id_clone
                {
                    continue;
                }

                match method {
                    "item/agentMessage/delta" => {
                        let delta = frame
                            .get("params")
                            .and_then(|p| p.get("delta"))
                            .and_then(|d| d.as_str())
                            .unwrap_or("");
                        if !delta.is_empty()
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
                        let err_text = frame
                            .get("params")
                            .and_then(|p| p.get("error"))
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "turn/failed without error payload".into());
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
                    | "account/rateLimits/updated"
                    | "turn/started" => {}
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
    if let Value::Object(map) = payload {
        map.entry("method".to_string())
            .or_insert_with(|| Value::String(method.to_string()));
        map.entry("type".to_string())
            .or_insert_with(|| Value::String(method.replace('/', ".")));
    }
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
    let is_approval = method.contains("requestApproval") || method.contains("approval");
    if !is_approval {
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
            Component::ParentDir => match out.components().last() {
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
