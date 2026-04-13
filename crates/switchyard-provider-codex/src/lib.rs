mod probe;
mod turn;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use switchyard_provider_api::*;
use switchyard_provider_subprocess::{effective_timeout_secs, resolve_command};

/// Codex provider adapter. Uses `codex` CLI in headless mode.
pub struct CodexProvider {
    /// Original configured command as provided by config/defaults.
    pub original_command: String,
    /// Resolved path/name of the codex command.
    pub command: String,
    pub args: Vec<String>,
    pub timeout_secs: u64,
    /// Stores results from start_turn for later finalize_turn.
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
}

impl CodexProvider {
    pub fn new(command: impl Into<String>, args: Vec<String>, timeout_secs: u64) -> Self {
        let original_command = command.into();
        let command = resolve_command(&original_command);
        Self {
            original_command,
            command,
            args,
            timeout_secs,
            results: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn from_config(cfg: &switchyard_config::ProviderConfig) -> Self {
        Self::new(cfg.command.clone(), cfg.args.clone(), cfg.timeout_secs)
    }
}

#[async_trait]
impl Provider for CodexProvider {
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
        let result = turn::run_codex_turn(
            turn_id,
            &self.original_command,
            &self.command,
            &self.args,
            &input,
            timeout_secs,
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
}
