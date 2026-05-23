//! Antigravity CLI (`agy`) adapter.
//!
//! Antigravity is Google's designated successor to the Gemini CLI. As of
//! 2026-05-21 its CLI surface is significantly thinner than Gemini's: there
//! is no `--output-format`, no `--acp`, no `stream-json`, no `--session-id`
//! analogue, no `--model`. Headless execution is one-shot plain-text via
//! `agy -p "<prompt>"`. See `docs/research/CLI_SESSION_SEMANTICS_2026-05-21.md`
//! for the full capability matrix.
//!
//! Implications for this adapter:
//!
//! - **No `LiveInstance`** — there is no streaming IPC channel to keep open.
//!   Each Switchyard turn is a fresh `agy -p` subprocess. `as_persistent`
//!   returns `None`; the `PersistentProviderProxy` falls through to the
//!   per-turn execution path.
//! - **Context lives in the prompt, not in the daemon** — Switchyard's
//!   canonical store is the source of truth. `compose_prompt` folds prior
//!   turns / session summary into the user message before each `agy -p`
//!   invocation, so we don't depend on Antigravity's opaque `.pb` session
//!   files at `~/.gemini/antigravity-cli/conversations/<uuid>.pb`.
//! - **Resume path deferred** — Antigravity exposes `-c` (continue most
//!   recent in cwd) and `--conversation <id>`, but the id is not surfaced by
//!   the CLI itself (Antigravity issue #7). When that lands, we can add a
//!   warmpath that reads `~/.gemini/antigravity-cli/cache/last_conversations.json`
//!   and re-uses the id across Switchyard turns within the same session.
//!
//! When upstream ships ACP (issue #31) the adapter can grow a JSON-RPC over
//! stdio path similar to the Codex `app_server` module.

mod probe;
mod turn;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use switchyard_provider_api::*;
use switchyard_provider_subprocess::{effective_timeout_secs, resolve_command};

pub struct AntigravityProvider {
    /// Original configured command as provided by config/defaults.
    pub original_command: String,
    /// Resolved path/name of the agy command. On Windows this typically
    /// resolves to `%LOCALAPPDATA%\agy\bin\agy.exe`; on macOS/Linux usually
    /// `~/.local/bin/agy` placed by the install script.
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub timeout_secs: u64,
    /// Per-turn results stash used by `finalize_turn` to hand back the
    /// composed `TurnResult` after `start_turn` completes.
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
}

impl AntigravityProvider {
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        env: HashMap<String, String>,
        timeout_secs: u64,
    ) -> Self {
        let original_command = command.into();
        let command = resolve_command(&original_command);
        Self {
            original_command,
            command,
            args,
            env,
            timeout_secs,
            results: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn from_config(cfg: &switchyard_config::ProviderConfig) -> Self {
        Self::new(
            cfg.command.clone(),
            cfg.args.clone(),
            cfg.env.clone(),
            cfg.timeout_secs,
        )
    }
}

#[async_trait]
impl Provider for AntigravityProvider {
    async fn probe(&self) -> Result<ProbeResult, ProviderError> {
        probe::run_probe(&self.command).await
    }

    async fn start_turn(
        &self,
        turn_id: Uuid,
        input: TurnInput,
        policy: ExecutionPolicy,
        _context: ContextBundle,
        event_tx: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<(), ProviderError> {
        let timeout_secs = effective_timeout_secs(self.timeout_secs, policy.timeout_secs);
        let result = turn::run_antigravity_turn(
            turn_id,
            &self.original_command,
            &self.command,
            &self.args,
            &input,
            &policy,
            timeout_secs,
            Some(&self.env),
            Some(&policy.cwd),
            &event_tx,
            cancel,
        )
        .await?;

        self.results.lock().await.insert(turn_id, result);
        Ok(())
    }

    async fn finalize_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
        self.results
            .lock()
            .await
            .remove(&turn_id)
            .ok_or_else(|| ProviderError::ExecutionFailed(format!("no result for turn {turn_id}")))
    }

    // Deliberately no `as_persistent` override. Antigravity has no streaming
    // IPC verb today (no `stream-json`, no `--acp`). The default `None` makes
    // `PersistentProviderProxy` fall through to per-turn subprocess execution.
}
