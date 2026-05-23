use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    ArtifactBundle, ContextBundle, ExecutionPolicy, LabelConflict, ProbeResult, ProviderError,
    ProviderEvent, TurnInput, TurnResult,
};

#[async_trait]
pub trait Provider: Send + Sync {
    async fn probe(&self) -> Result<ProbeResult, ProviderError>;

    /// Start a turn. The caller provides the canonical `turn_id` so that
    /// all ProviderEvents emitted through `event_tx` can be correlated
    /// back to the correct turn — even when multiple turns run concurrently
    /// across core and peer providers.
    ///
    /// `cancel` is signalled when the user requests cancellation (e.g. Esc).
    /// Implementations should select on `cancel.cancelled()` alongside their
    /// work and abort promptly (kill subprocesses, etc.) when triggered.
    async fn start_turn(
        &self,
        turn_id: Uuid,
        input: TurnInput,
        policy: ExecutionPolicy,
        context: ContextBundle,
        event_tx: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<(), ProviderError>;

    /// Finalize the turn identified by `turn_id`.
    /// The provider must use this to collect the final result and artifacts
    /// for the specific turn, not rely on hidden internal state.
    async fn finalize_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<(TurnResult, ArtifactBundle), ProviderError>;

    fn as_persistent(&self) -> Option<&dyn PersistentProvider> {
        None
    }
}

/// Providers that can be kept alive between turns expose this trait.
/// The pool / WorkerSupervisor calls `start_persistent_instance` to spawn
/// raw long-lived processes; the resulting [`LiveInstance`] has no identity
/// (instance_id, label, session affiliation) — identity is attached at
/// [`LiveInstanceRegistry::register`] time.
#[async_trait]
pub trait PersistentProvider: Send + Sync {
    async fn start_persistent_instance(
        &self,
        cwd: PathBuf,
        envs: HashMap<String, String>,
    ) -> Result<Box<dyn LiveInstance>, ProviderError>;

    /// Attempt to start an instance bound to a prior session handle so the
    /// CLI daemon resumes the existing conversation rather than starting
    /// fresh. `resume_token` is the opaque string previously returned by
    /// [`LiveInstance::resume_token`] — typically a thread / session id.
    ///
    /// Default implementation discards the token and falls through to
    /// [`start_persistent_instance`], which is the correct behaviour for
    /// providers without a resume verb (e.g. Antigravity). Providers with
    /// real resume support (Codex via `thread/resume`) override this and
    /// gracefully fall back to a fresh start if the daemon refuses the
    /// resume (token expired, format change, etc.).
    async fn start_persistent_instance_resumed(
        &self,
        cwd: PathBuf,
        envs: HashMap<String, String>,
        _resume_token: Option<String>,
    ) -> Result<Box<dyn LiveInstance>, ProviderError> {
        self.start_persistent_instance(cwd, envs).await
    }
}

#[async_trait]
pub trait LiveInstance: Send + Sync {
    async fn send_message(
        &mut self,
        text: &str,
    ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError>;

    /// Send a turn with an explicit policy applied to any server-initiated
    /// approval requests (Codex `requestApproval` JSON-RPC frames). Tool
    /// invocations writing outside `policy.allowed_paths`, or any write
    /// when `policy.write_access = false`, are denied with the daemon
    /// receiving `{decision: "deny"}` rather than the default
    /// `{decision: "approve"}`.
    ///
    /// Default implementation delegates to [`send_message`] — providers
    /// without daemon-side approval prompts (Claude per-turn, Antigravity)
    /// have nothing to gate, so the policy parameter is harmless.
    async fn send_message_with_policy(
        &mut self,
        text: &str,
        _policy: &crate::ExecutionPolicy,
    ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
        self.send_message(text).await
    }

    /// Send a full turn payload, including optional local image attachments,
    /// through a persistent instance. Providers with native multimodal daemon
    /// support should override this method. The default path preserves
    /// backward compatibility by appending attachment file references to the
    /// text and delegating to `send_message_with_policy`.
    async fn send_turn_with_policy(
        &mut self,
        input: &TurnInput,
        policy: &crate::ExecutionPolicy,
    ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
        let text = input.user_message_with_attachment_references();
        self.send_message_with_policy(&text, policy).await
    }
    async fn update_context(&mut self, context: ContextBundle) -> Result<(), ProviderError>;
    async fn terminate(&mut self) -> Result<(), ProviderError>;
    fn is_healthy(&mut self) -> bool {
        true
    }
    /// Opaque token that can be passed back to
    /// [`PersistentProvider::start_persistent_instance_resumed`] on a future
    /// spawn so the daemon resumes the same conversation. For Codex this is
    /// the JSON-RPC `thread.id`; for Claude it's the `--session-id`. Returns
    /// `None` for instances without a resume mechanism.
    fn resume_token(&self) -> Option<String> {
        None
    }

    /// Rewind the daemon's internal conversation to the point immediately
    /// before the user message at `turn_index` (0-based, counting user
    /// messages only — assistant responses don't contribute to the index).
    /// On success the instance is ready to receive a new `send_message`
    /// that continues from that earlier point as if subsequent turns had
    /// never happened.
    ///
    /// Used by the edit / retry UX to perform a warm fork instead of the
    /// degraded "terminate the daemon, wipe the canonical tail, respawn from
    /// scratch" dance. Default implementation returns
    /// [`ProviderError::UnsupportedCapability`]; Codex overrides via its
    /// `thread/fork` JSON-RPC verb. Callers should fall back to the cold
    /// rewind path when this returns `UnsupportedCapability`.
    async fn rewind_to(&mut self, turn_index: u32) -> Result<(), ProviderError> {
        let _ = turn_index;
        Err(ProviderError::UnsupportedCapability(
            "warm rewind not supported by this LiveInstance".into(),
        ))
    }
}

#[async_trait]
impl<T: ?Sized + LiveInstance> LiveInstance for Box<T> {
    async fn send_message(
        &mut self,
        text: &str,
    ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
        (**self).send_message(text).await
    }
    async fn send_message_with_policy(
        &mut self,
        text: &str,
        policy: &crate::ExecutionPolicy,
    ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
        (**self).send_message_with_policy(text, policy).await
    }
    async fn send_turn_with_policy(
        &mut self,
        input: &TurnInput,
        policy: &crate::ExecutionPolicy,
    ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
        (**self).send_turn_with_policy(input, policy).await
    }
    async fn update_context(&mut self, context: ContextBundle) -> Result<(), ProviderError> {
        (**self).update_context(context).await
    }
    async fn terminate(&mut self) -> Result<(), ProviderError> {
        (**self).terminate().await
    }
    fn is_healthy(&mut self) -> bool {
        (**self).is_healthy()
    }
    fn resume_token(&self) -> Option<String> {
        (**self).resume_token()
    }
    async fn rewind_to(&mut self, turn_index: u32) -> Result<(), ProviderError> {
        (**self).rewind_to(turn_index).await
    }
}

/// What role this instance plays in its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceKind {
    /// The session's main user-facing provider. Exactly one per session.
    Core,
    /// A team worker spawned by the Core via delegation. Zero or more per session.
    Worker,
}

/// Lifecycle state of a registered instance. Owned by the registry / pool;
/// supervisor mutates via [`LiveInstanceRegistry::update_state`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
    /// Spawn in progress — process exists but not yet ready to accept input.
    Spawning,
    /// Healthy and available for checkout.
    Idle,
    /// Currently handling the given turn. Only one turn at a time per instance.
    Busy { turn_id: Uuid },
    /// Mid-turn died; supervisor is replaying input on a fresh process.
    Retrying,
    /// Terminate sequence in flight (stdin closed, awaiting child exit).
    Dying,
    /// Child process exited and was reaped. About to be removed from pool.
    Dead,
}

/// Identity and lifecycle metadata for a registered instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceMetadata {
    /// Auto-generated. The unambiguous handle the registry stores under.
    pub instance_id: Uuid,
    /// Provider name (e.g. "claude", "codex"). Same scope as the registry key.
    pub provider: String,
    /// Switchyard session this instance is bound to. Cross-session sharing is
    /// not allowed — the pool partitions buckets by `(provider, session_id)`.
    pub session_id: Uuid,
    /// Optional semantic name set by the spawner (typically the Core's
    /// delegation request id like `claude-project-structure-map`). Unique
    /// within `(provider, session_id)` — duplicates raise [`LabelConflict`].
    pub label: Option<String>,
    pub kind: InstanceKind,
    pub spawned_at: DateTime<Utc>,
    pub state: InstanceState,
}

impl InstanceMetadata {
    /// Build a fresh metadata record with a new `instance_id` and `Spawning`
    /// state. Callers flip to [`InstanceState::Idle`] once spawn completes.
    pub fn new(
        provider: impl Into<String>,
        session_id: Uuid,
        label: Option<String>,
        kind: InstanceKind,
    ) -> Self {
        Self {
            instance_id: Uuid::now_v7(),
            provider: provider.into(),
            session_id,
            label,
            kind,
            spawned_at: Utc::now(),
            state: InstanceState::Spawning,
        }
    }
}

pub trait LiveInstanceRegistry: Send + Sync {
    /// Register an instance with full identity metadata. Returns the
    /// `instance_id` on success, or [`LabelConflict`] if a labelled instance
    /// already exists at the same `(provider, session_id, label)` triple.
    fn register(
        &self,
        metadata: InstanceMetadata,
        instance: Box<dyn LiveInstance>,
    ) -> Result<Uuid, LabelConflict>;

    /// Check out by exact `instance_id`. Returns `None` if the id isn't
    /// registered or the instance isn't in `Idle` state. On success the
    /// instance state transitions to `Busy { turn_id: Uuid::nil() }`; callers
    /// should follow up with [`update_state`] once a real turn_id is known.
    fn checkout_by_id(
        &self,
        instance_id: Uuid,
    ) -> Option<Arc<tokio::sync::Mutex<dyn LiveInstance>>>;

    /// Check out by semantic label within a session. Returns `None` if no
    /// matching instance exists or all matches are busy.
    fn checkout_by_label(
        &self,
        provider: &str,
        session_id: Uuid,
        label: &str,
    ) -> Option<Arc<tokio::sync::Mutex<dyn LiveInstance>>>;

    /// Check out any idle instance for `(provider, session_id)`. Returns the
    /// matched `instance_id` so the caller can release / update_state later.
    fn checkout_any_idle(
        &self,
        provider: &str,
        session_id: Uuid,
    ) -> Option<(Uuid, Arc<tokio::sync::Mutex<dyn LiveInstance>>)>;

    /// Transition `instance_id` back to `Idle`. No-op if the id is unknown
    /// or already removed (e.g. concurrent terminate). Pool retains the
    /// underlying Arc — the caller does not need to hand it back.
    fn release(&self, instance_id: Uuid);

    /// Whether any instance is registered for `(provider, session_id)`,
    /// regardless of its current state.
    fn has_live_instance(&self, provider: &str, session_id: Uuid) -> bool;

    /// Snapshot of all instances bound to a session (across providers).
    /// Used by GUI to render the worker roster.
    fn list_session(&self, session_id: Uuid) -> Vec<InstanceMetadata>;

    /// Update lifecycle state for an arbitrary transition (Spawning→Idle,
    /// Idle→Retrying, Retrying→Idle, *→Dead). No-op for unknown ids.
    fn update_state(&self, instance_id: Uuid, state: InstanceState);

    /// Remove the instance from the pool and terminate its underlying
    /// process. Idempotent — terminate of an unknown id is a no-op.
    fn terminate(&self, instance_id: Uuid);
}
