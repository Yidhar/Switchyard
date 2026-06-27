//! KohakuTerrarium (`kt`) CLI adapter.
//!
//! Drives `kt run <creature> --headless --json -p <prompt>` as a one-shot
//! subprocess and maps its JSONL event stream onto [`ProviderEvent`]s. This
//! requires a `kt` build that supports the headless mode (the
//! `switchyard-headless` fork); [`probe`] detects it via the capability line
//! `kt --version` prints.
//!
//! v1 is one-shot per turn (no [`PersistentProvider`]); multi-turn continuity
//! is achieved by resuming the same `--session <path>.kohakutr` each turn.

mod jsonl;
mod probe;
mod turn;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use switchyard_provider_api::*;
use switchyard_provider_subprocess::{effective_timeout_secs, resolve_command};

pub struct KohakuProvider {
    pub original_command: String,
    pub command: String,
    /// Configured args — the FIRST entry must be the creature/recipe ref
    /// (e.g. `@kt_biome/coder` or a config-folder path); it becomes the
    /// `agent_path` positional after `kt run`.
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    /// Maps onto the `kt --llm <selector>` profile (e.g. `enzi/gpt-5.5-custom`).
    pub model: Option<String>,
    /// Reserved: KT has no standalone effort flag; reasoning is encoded in the
    /// `--llm` selector's `@reasoning=` variation, so this is not mapped in v1.
    pub thinking_level: Option<String>,
    pub timeout_secs: u64,
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
}

impl KohakuProvider {
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        env: HashMap<String, String>,
        timeout_secs: u64,
    ) -> Self {
        Self::new_with_options(command, args, env, timeout_secs, None, None)
    }

    pub fn new_with_options(
        command: impl Into<String>,
        args: Vec<String>,
        env: HashMap<String, String>,
        timeout_secs: u64,
        model: Option<String>,
        thinking_level: Option<String>,
    ) -> Self {
        let original_command = command.into();
        let command = resolve_command(&original_command);
        Self {
            original_command,
            command,
            args,
            env,
            model,
            thinking_level,
            timeout_secs,
            results: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn from_config(cfg: &switchyard_config::ProviderConfig) -> Self {
        Self::new_with_options(
            cfg.command.clone(),
            cfg.args.clone(),
            cfg.env.clone(),
            cfg.timeout_secs,
            cfg.model.clone(),
            cfg.thinking_level.clone(),
        )
    }
}

#[async_trait]
impl Provider for KohakuProvider {
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
        let result = turn::run_kohaku_turn(
            turn_id,
            &self.original_command,
            &self.command,
            &self.args,
            self.model.as_deref(),
            self.thinking_level.as_deref(),
            &input,
            timeout_secs,
            Some(&self.env),
            &policy,
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

    fn as_persistent(&self) -> Option<&dyn PersistentProvider> {
        // v1: one-shot per turn. Multi-turn continuity is via `--session`
        // resume rather than a long-lived process.
        None
    }
}
