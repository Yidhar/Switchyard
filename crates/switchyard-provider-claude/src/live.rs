//! Persistent Claude Code instance speaking the stream-json IO protocol.
//!
//! Spawn invariant:
//!
//! ```text
//! claude --print --input-format stream-json --output-format stream-json \
//!        --session-id <v7-uuid> --include-partial-messages --verbose
//! ```
//!
//! One `claude` process stays alive across many Switchyard turns. Each
//! [`send_message`] writes one stream-json user message to stdin; the turn
//! ends when claude emits its `result` event on stdout.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc};
use tokio::time::sleep;
use uuid::Uuid;

use switchyard_provider_api::{
    ContextBundle, EventType, LiveInstance, ProviderError, ProviderEvent,
};

use crate::stream_json::extract_delta_text;

/// Long-running Claude Code process driven via newline-delimited stream-json IO.
pub struct ClaudeLiveInstance {
    /// Switchyard-allocated session id used as `--session-id` for claude.
    /// The on-disk transcript lands at
    /// `~/.claude/projects/<cwd-hash>/<session_id>.jsonl`.
    pub session_id: Uuid,
    pub child: Child,
    pub stdin: ChildStdin,
    /// Stdout lines arrive here in claude's emit order. Wrapped in a Mutex so
    /// the per-turn consumer task can lock it for one turn's duration.
    pub stdout_rx: Arc<Mutex<mpsc::Receiver<String>>>,
}

impl ClaudeLiveInstance {
    /// Spawn a persistent claude instance bound to `cwd` with a fresh
    /// session id. Equivalent to `spawn_with_resume(..., None)`; kept as a
    /// thin wrapper so existing call-sites and tests don't churn.
    ///
    /// `command` is the resolved claude executable. `extra_args` are appended
    /// after the stream-json invariants; callers must NOT pass conflicting
    /// flags (`-p` / `--print` / `--input-format` / `--output-format` /
    /// `--session-id`) or claude will error on duplicates.
    pub async fn spawn(
        command: &str,
        extra_args: &[String],
        env: HashMap<String, String>,
        cwd: Option<&std::path::Path>,
    ) -> Result<Self, ProviderError> {
        Self::spawn_with_resume(command, extra_args, env, cwd, None).await
    }

    /// Spawn a persistent claude instance, optionally re-using a prior
    /// `--session-id`. When the supplied id matches an on-disk transcript at
    /// `~/.claude/projects/<cwd-hash>/<session_id>.jsonl`, claude resumes it
    /// and the agent sees the full prior conversation. When the file is
    /// absent (deleted, fresh project), claude still comes up — just without
    /// prior context — so this call never fails because of a stale token.
    /// Callers persist [`Self::session_id`] after spawn so the next respawn
    /// can resume cleanly.
    pub async fn spawn_with_resume(
        command: &str,
        extra_args: &[String],
        env: HashMap<String, String>,
        cwd: Option<&std::path::Path>,
        resume_session_id: Option<&str>,
    ) -> Result<Self, ProviderError> {
        let session_id = match resume_session_id.and_then(|s| Uuid::parse_str(s).ok()) {
            Some(id) => id,
            None => Uuid::now_v7(),
        };

        let mut args: Vec<String> = vec![
            "--print".into(),
            "--input-format".into(),
            "stream-json".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--session-id".into(),
            session_id.to_string(),
            "--include-partial-messages".into(),
            "--verbose".into(),
        ];
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
            ProviderError::ExecutionFailed(format!("failed to spawn claude live: {e}"))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            ProviderError::ExecutionFailed("claude live: stdin missing after spawn".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ProviderError::ExecutionFailed("claude live: stdout missing after spawn".into())
        })?;
        let stderr = child.stderr.take();

        // Drain stderr so claude never blocks on a full pipe. We don't try to
        // attach semantics to these lines — surface via tracing later if useful.
        if let Some(se) = stderr {
            tokio::spawn(async move {
                let mut reader = BufReader::new(se).lines();
                while let Ok(Some(_line)) = reader.next_line().await {}
            });
        }

        // Bridge stdout into a channel of NDJSON lines.
        let (stdout_tx, stdout_rx) = mpsc::channel::<String>(1024);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if stdout_tx.send(line).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            session_id,
            child,
            stdin,
            stdout_rx: Arc::new(Mutex::new(stdout_rx)),
        })
    }
}

#[async_trait]
impl LiveInstance for ClaudeLiveInstance {
    async fn send_message(
        &mut self,
        text: &str,
    ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
        // Encode the turn input as a stream-json user message. The wire shape
        // is the Anthropic Messages API user block — the spike verified the
        // CLI accepts this exact form.
        let msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{ "type": "text", "text": text }],
            },
        });
        let line = format!("{msg}\n");
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| ProviderError::ExecutionFailed(format!("write claude stdin: {e}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| ProviderError::ExecutionFailed(format!("flush claude stdin: {e}")))?;

        let (event_tx, event_rx) = mpsc::channel(256);
        let turn_id = Uuid::now_v7();
        let rx_lock = Arc::clone(&self.stdout_rx);

        tokio::spawn(async move {
            // Hold the shared stdout receiver for the duration of this turn so
            // overlapping send_message calls (which shouldn't happen — the
            // trait takes &mut self — but defensive) can't interleave events.
            let mut rx = rx_lock.lock().await;
            while let Some(line) = rx.recv().await {
                let json: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => {
                        // Non-JSON line — surface as plain text for debugging.
                        let _ = event_tx
                            .send(ProviderEvent::text_message(turn_id, "claude", line))
                            .await;
                        continue;
                    }
                };

                let msg_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");

                // 1. Streaming text deltas → text_message events. Handles both
                //    the wrapped form (`type=stream_event` containing a
                //    `content_block_delta` event, emitted with
                //    --include-partial-messages) and the unwrapped form
                //    (`type=content_block_delta` at top level).
                if let Some(delta_text) = extract_delta_text(&json, msg_type)
                    && !delta_text.is_empty()
                    && event_tx
                        .send(ProviderEvent::text_message(turn_id, "claude", delta_text))
                        .await
                        .is_err()
                {
                    return;
                }

                // 2. Drop the consolidated `assistant` event. Its content
                //    already streamed as deltas; emitting it again would
                //    double-render in text accumulators downstream.
                if msg_type == "assistant" {
                    continue;
                }

                // 3. `result` is the turn boundary. Emit TurnCompleted with
                //    the raw payload (carries usage / cost / num_turns), then
                //    close this turn's event channel by returning.
                if msg_type == "result" {
                    let _ = event_tx
                        .send(ProviderEvent::new(
                            turn_id,
                            EventType::TurnCompleted,
                            "claude",
                            json,
                        ))
                        .await;
                    return;
                }

                // 4. Everything else (system/init, system/status,
                //    rate_limit_event, stream_event subtypes other than
                //    content_block_delta) → ItemUpdated carrying the raw
                //    payload. Downstream uses extract_activity_summary for
                //    human-readable rendering.
                if event_tx
                    .send(ProviderEvent::new(
                        turn_id,
                        EventType::ItemUpdated,
                        "claude",
                        json,
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            // stdout closed before a `result` arrived — surface as turn
            // failure so the caller doesn't hang on the event channel.
            let _ = event_tx
                .send(ProviderEvent::turn_failed(
                    turn_id,
                    "claude",
                    "claude stdout closed before result event",
                ))
                .await;
        });

        Ok(event_rx)
    }

    async fn update_context(&mut self, _context: ContextBundle) -> Result<(), ProviderError> {
        // No-op: claude's stream-json `--input-format` has no `system` role
        // channel, and conversation context is already maintained by claude
        // itself across turns in this persistent instance (verified by the
        // spike: turn 2 saw cache_read_input_tokens=26890 vs turn 1's 19069).
        // Switchyard's Context Composer is expected to fold any extra context
        // into the user message body before calling `send_message`. Heavier
        // changes (a different --system-prompt) would require respawning the
        // instance.
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), ProviderError> {
        // Closing stdin lets claude exit cleanly (spike confirmed exit code 0).
        // Kill is the fallback if it still hasn't gone away after a grace.
        let _ = self.stdin.shutdown().await;
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
        // The `--session-id` we passed at spawn is what `--session-id <prior>`
        // wants on a future respawn. Claude resumes the on-disk transcript
        // at `~/.claude/projects/<cwd-hash>/<id>.jsonl` when the file
        // exists, otherwise mints a fresh session keyed by the same id —
        // either way the upper layer gets continuity by token reuse.
        Some(self.session_id.to_string())
    }
}

// `extract_delta_text` lives in [`crate::stream_json`] so the per-turn `-p`
// path can use the same parser without duplicating its branching logic.
