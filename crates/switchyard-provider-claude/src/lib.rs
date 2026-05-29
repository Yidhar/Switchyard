//! Claude Code CLI adapter.
//!
//! Two execution modes:
//! - Per-turn: `claude -p --output-format stream-json "prompt"` (see `turn`)
//! - Persistent: long-running `claude --print --input-format stream-json
//!   --output-format stream-json --session-id <uuid> --include-partial-messages
//!   --verbose` driven via stdin (see `live`)

mod live;
mod probe;
mod stream_json;
mod turn;

pub use live::ClaudeLiveInstance;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use switchyard_provider_api::*;
use switchyard_provider_subprocess::{effective_timeout_secs, resolve_command};

pub struct ClaudeProvider {
    pub original_command: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
    pub timeout_secs: u64,
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
}

impl ClaudeProvider {
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

    fn effective_args(&self) -> Vec<String> {
        let mut args =
            turn::claude_runtime_args(self.model.as_deref(), self.thinking_level.as_deref());
        args.extend(self.args.clone());
        args
    }
}

#[async_trait]
impl Provider for ClaudeProvider {
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
        let effective_args = self.effective_args();
        let result = turn::run_claude_turn(
            turn_id,
            &self.original_command,
            &self.command,
            &effective_args,
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
        Some(self)
    }
}

#[async_trait]
impl PersistentProvider for ClaudeProvider {
    async fn start_persistent_instance(
        &self,
        cwd: std::path::PathBuf,
        envs: HashMap<String, String>,
    ) -> Result<Box<dyn LiveInstance>, ProviderError> {
        let effective_args = self.effective_args();
        let instance =
            ClaudeLiveInstance::spawn(&self.command, &effective_args, envs, Some(&cwd)).await?;
        Ok(Box::new(instance))
    }

    async fn start_persistent_instance_resumed(
        &self,
        cwd: std::path::PathBuf,
        envs: HashMap<String, String>,
        resume_token: Option<String>,
    ) -> Result<Box<dyn LiveInstance>, ProviderError> {
        // Pass the prior `--session-id` through. When the on-disk transcript
        // exists, claude resumes the conversation seamlessly; when it's
        // missing (project moved, transcript deleted), claude still spawns
        // with the same id but no prior context. Either way the call never
        // fails because of a stale token, matching the Codex G semantics.
        let effective_args = self.effective_args();
        let instance = ClaudeLiveInstance::spawn_with_resume(
            &self.command,
            &effective_args,
            envs,
            Some(&cwd),
            resume_token.as_deref(),
        )
        .await?;
        Ok(Box::new(instance))
    }
}
