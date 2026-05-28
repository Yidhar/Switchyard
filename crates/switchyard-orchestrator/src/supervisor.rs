//! `WorkerSupervisor` — owns the spawn / retry / drain / death-notify
//! lifecycle for a single Core-issued delegation.
//!
//! Why this exists: providers know how to spawn a single live instance, the
//! pool knows how to track many of them, but **neither** knows what to do
//! when a worker dies mid-turn. The supervisor closes that gap:
//!
//! - find an existing labelled worker (or spawn a new one)
//! - drive a turn, forwarding events with `instance_id` + `label` stamps
//! - on mid-turn death, terminate the corpse, wait the configured backoff,
//!   respawn under the same label, and replay the user input
//! - after `max_attempts` failures, emit a synthesized `TurnFailed` event
//!   carrying `error_kind="worker_died_permanently"` through the same event
//!   channel so the Core sees it via its normal `delegate_completed` path
//!
//! The supervisor does NOT emit `delegate_retrying` mid-flight — by user
//! requirement the Core stays oblivious to retry mechanics; only the UI
//! (when subscribed via the runtime event channel) sees retry progress.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use switchyard_config::WorkerRetryConfig;
use switchyard_provider_api::{
    ContextBundle, EventType, ExecutionPolicy, InstanceKind, InstanceMetadata, InstanceState,
    LiveInstanceRegistry, Provider, ProviderError, ProviderEvent,
};

/// Lifecycle event emitted by [`WorkerSupervisor`] for UI consumption.
/// Switchyard-core wraps these into `RuntimeEvent::Worker*` variants;
/// orchestrator can't depend on core (would cycle) so we keep our own
/// enum and let the caller translate.
#[derive(Debug, Clone)]
pub enum SupervisorLifecycleEvent {
    /// A worker process was just spawned and registered in the pool.
    Spawned {
        session_id: Uuid,
        instance_id: Uuid,
        provider: String,
        label: Option<String>,
        /// "core" or "worker" — matches `InstanceKind` serialization.
        kind: String,
        spawned_at: String,
    },
    /// A worker's pool state transitioned (idle↔busy↔retrying↔dying).
    StateChanged {
        session_id: Uuid,
        instance_id: Uuid,
        state: String,
        in_flight_turn_id: Option<Uuid>,
    },
    /// Supervisor is about to retry after a mid-turn death. `attempt` is the
    /// 1-indexed retry number (i.e. attempt=1 = first retry after initial
    /// failure). Core does not see this — UI only.
    Retrying {
        session_id: Uuid,
        instance_id: Option<Uuid>,
        provider: String,
        label: Option<String>,
        attempt: u32,
        last_error: String,
    },
    /// A worker was removed from the pool.
    Terminated {
        session_id: Uuid,
        instance_id: Uuid,
        provider: String,
        label: Option<String>,
        /// `released`, `completed_use_once`, `died_mid_turn`, `permanent_death`,
        /// `core_reset`, `session_clear`.
        reason: String,
    },
}

/// Callback the supervisor invokes for each lifecycle event. The orchestrator
/// hands one of these in at construction; the closure typically forwards into
/// a `RuntimeEvent` channel owned by the GUI/TUI layer.
pub type SupervisorObserver = dyn Fn(SupervisorLifecycleEvent) + Send + Sync;

const LOST_INPUT_PREVIEW_CHARS: usize = 200;

/// Configurable retry behaviour. Built from [`WorkerRetryConfig`] in the
/// loaded `switchyard.toml`. `max_attempts` counts the initial try plus
/// retries — `max_attempts = 3` means up to 2 retries after the first failure.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: Vec<Duration>,
}

impl RetryPolicy {
    pub fn from_config(cfg: &WorkerRetryConfig) -> Self {
        Self {
            max_attempts: cfg.max_attempts.max(1),
            backoff: cfg
                .backoff_ms
                .iter()
                .copied()
                .map(Duration::from_millis)
                .collect(),
        }
    }

    /// How long to wait before attempt number `attempt_idx` (0 = no wait).
    /// Indices beyond the configured backoff vector saturate at the last entry.
    fn backoff_for(&self, attempt_idx: u32) -> Duration {
        if attempt_idx == 0 || self.backoff.is_empty() {
            return Duration::ZERO;
        }
        let idx = (attempt_idx as usize - 1).min(self.backoff.len() - 1);
        self.backoff[idx]
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::from_config(&WorkerRetryConfig::default())
    }
}

/// Everything needed to spawn (and later respawn) a worker, plus the Core's
/// reuse-after-completion preference.
#[derive(Clone)]
pub struct SpawnRecipe {
    pub provider: String,
    pub session_id: Uuid,
    pub label: Option<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    /// If true (default), the worker stays in the pool as Idle after a
    /// successful turn — the next matching delegation reuses it. If false,
    /// the supervisor terminates the worker after the turn completes.
    pub reuse_after: bool,
    /// Optional opaque resume token for the worker's CLI daemon. When
    /// `Some`, the supervisor calls `start_persistent_instance_resumed`
    /// instead of the fresh-start variant — Codex's app-server resumes
    /// the prior thread, Claude reuses its `--session-id`. None for
    /// brand-new workers (the common case); populated by the
    /// orchestrator on retry from the most recent worker's
    /// `resume_token()`.
    pub resume_token: Option<String>,
}

/// Result returned to the orchestrator after the supervisor finishes.
pub struct SupervisedOutcome {
    pub response_text: String,
    /// True if the turn ended in a failure state — either an in-turn failure
    /// signalled by the provider, or a synthesized permanent-death failure.
    pub failed: bool,
    /// 0 = succeeded on the first attempt. N>0 = N retries before giving up
    /// or succeeding.
    pub retries_attempted: u32,
    pub last_error: Option<String>,
    /// `Some` when the worker is still alive at function exit (and registered
    /// in the pool, transitioned back to Idle). `None` when the worker is
    /// gone — either permanently dead or terminated due to `reuse_after=false`
    /// / mid-turn failure.
    pub final_instance_id: Option<Uuid>,
}

pub struct WorkerSupervisor {
    /// Held as `Arc<dyn …>` rather than `&dyn …` so the supervisor can be
    /// cloned into `tokio::spawn`-ed task futures without lifetime gymnastics.
    /// The pool itself is `Arc<InstancePool>` everywhere upstream, so this
    /// reuses the existing reference-counting shape.
    registry: Arc<dyn LiveInstanceRegistry>,
    retry_policy: RetryPolicy,
    /// Optional callback for lifecycle events. The orchestrator wraps a closure
    /// here that translates [`SupervisorLifecycleEvent`] into the GUI/TUI's
    /// runtime event channel.
    observer: Option<Arc<SupervisorObserver>>,
}

impl WorkerSupervisor {
    pub fn new(
        registry: Arc<dyn LiveInstanceRegistry>,
        retry_policy: RetryPolicy,
        observer: Option<Arc<SupervisorObserver>>,
    ) -> Self {
        Self {
            registry,
            retry_policy,
            observer,
        }
    }

    fn emit(&self, event: SupervisorLifecycleEvent) {
        if let Some(obs) = &self.observer {
            obs(event);
        }
    }

    /// Drive a single delegation under retry-on-death policy.
    ///
    /// `provider` is the freshly-resolved Provider implementation for the
    /// recipe's provider name — the supervisor uses it to spawn (and respawn)
    /// live instances. It must implement `PersistentProvider`; otherwise the
    /// supervisor returns immediately with `worker_died_permanently`.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        provider: &dyn Provider,
        recipe: SpawnRecipe,
        turn_id: Uuid,
        input_text: String,
        context: ContextBundle,
        event_tx: mpsc::Sender<ProviderEvent>,
        policy: ExecutionPolicy,
        cancel: CancellationToken,
    ) -> SupervisedOutcome {
        // Persistence is the supervisor's whole reason to exist — refuse non-
        // persistent providers loudly rather than silently falling back.
        let persistent = match provider.as_persistent() {
            Some(p) => p,
            None => {
                return self
                    .emit_permanent_failure(
                        &recipe,
                        turn_id,
                        &event_tx,
                        0,
                        Some(format!(
                            "provider {} does not implement PersistentProvider",
                            recipe.provider
                        )),
                        &input_text,
                    )
                    .await;
            }
        };

        let mut last_error: Option<String> = None;

        for attempt in 0..self.retry_policy.max_attempts {
            // Backoff before retry attempts. Attempt 0 = no wait.
            let wait = self.retry_policy.backoff_for(attempt);
            if !wait.is_zero() {
                tokio::select! {
                    _ = sleep(wait) => {}
                    _ = cancel.cancelled() => {
                        return self
                            .emit_permanent_failure(
                                &recipe, turn_id, &event_tx, attempt,
                                Some("cancelled during retry backoff".into()),
                                &input_text,
                            )
                            .await;
                    }
                }
            }

            // 1) Acquire a worker — labelled checkout if the recipe has one
            //    AND it's already in the pool; otherwise spawn fresh.
            let acquired = match self.acquire_worker(persistent, &recipe).await {
                Ok(pair) => pair,
                Err(e) => {
                    let err_msg = format!("spawn failed: {e}");
                    last_error = Some(err_msg.clone());
                    // Spawn itself failed; emit Retrying (without instance_id)
                    // unless this was the final attempt.
                    if attempt + 1 < self.retry_policy.max_attempts {
                        self.emit(SupervisorLifecycleEvent::Retrying {
                            session_id: recipe.session_id,
                            instance_id: None,
                            provider: recipe.provider.clone(),
                            label: recipe.label.clone(),
                            attempt: attempt + 1,
                            last_error: err_msg,
                        });
                    }
                    continue;
                }
            };
            let (instance_id, inst_lock) = acquired;

            // Mark Busy in the pool so list_session snapshots are accurate even
            // before the first event reaches the UI, and emit StateChanged
            // for live subscribers.
            self.registry
                .update_state(instance_id, InstanceState::Busy { turn_id });
            self.emit(SupervisorLifecycleEvent::StateChanged {
                session_id: recipe.session_id,
                instance_id,
                state: "busy".to_string(),
                in_flight_turn_id: Some(turn_id),
            });

            // 2) Drive the turn.
            let attempt_outcome = self
                .run_attempt(
                    instance_id,
                    inst_lock,
                    turn_id,
                    &recipe,
                    &input_text,
                    context.clone(),
                    &event_tx,
                    &policy,
                    cancel.clone(),
                )
                .await;

            match attempt_outcome {
                AttemptResult::Completed {
                    response_text,
                    in_turn_failure,
                } => {
                    // Worker is still alive. Either keep in pool (reuse) or
                    // terminate (use-once / in-turn failure).
                    if recipe.reuse_after && !in_turn_failure {
                        self.registry.release(instance_id);
                        self.emit(SupervisorLifecycleEvent::StateChanged {
                            session_id: recipe.session_id,
                            instance_id,
                            state: "idle".to_string(),
                            in_flight_turn_id: None,
                        });
                        return SupervisedOutcome {
                            response_text,
                            failed: false,
                            retries_attempted: attempt,
                            last_error: None,
                            final_instance_id: Some(instance_id),
                        };
                    }
                    self.registry.terminate(instance_id);
                    self.emit(SupervisorLifecycleEvent::Terminated {
                        session_id: recipe.session_id,
                        instance_id,
                        provider: recipe.provider.clone(),
                        label: recipe.label.clone(),
                        reason: if in_turn_failure {
                            "died_mid_turn".to_string()
                        } else {
                            "completed_use_once".to_string()
                        },
                    });
                    return SupervisedOutcome {
                        response_text,
                        failed: in_turn_failure,
                        retries_attempted: attempt,
                        last_error: if in_turn_failure {
                            Some("provider signalled in-turn failure".into())
                        } else {
                            None
                        },
                        final_instance_id: None,
                    };
                }
                AttemptResult::DiedMidTurn { error, .. } => {
                    last_error = Some(error.clone());
                    // Corpse must leave the pool before we respawn under the
                    // same label; otherwise register() would hit LabelConflict.
                    self.registry.terminate(instance_id);
                    self.emit(SupervisorLifecycleEvent::Terminated {
                        session_id: recipe.session_id,
                        instance_id,
                        provider: recipe.provider.clone(),
                        label: recipe.label.clone(),
                        reason: "died_mid_turn".to_string(),
                    });
                    // Emit Retrying unless we've used all attempts.
                    if attempt + 1 < self.retry_policy.max_attempts {
                        self.emit(SupervisorLifecycleEvent::Retrying {
                            session_id: recipe.session_id,
                            instance_id: Some(instance_id),
                            provider: recipe.provider.clone(),
                            label: recipe.label.clone(),
                            attempt: attempt + 1,
                            last_error: error,
                        });
                    }
                    // Loop continues — next iteration retries.
                }
            }
        }

        // Retries exhausted.
        self.emit_permanent_failure(
            &recipe,
            turn_id,
            &event_tx,
            self.retry_policy.max_attempts,
            last_error,
            &input_text,
        )
        .await
    }

    /// Find a matching labelled worker in the pool, or spawn a new one.
    /// Always returns an instance handle that is in `Busy` state.
    async fn acquire_worker(
        &self,
        persistent: &dyn switchyard_provider_api::PersistentProvider,
        recipe: &SpawnRecipe,
    ) -> Result<
        (
            Uuid,
            std::sync::Arc<tokio::sync::Mutex<dyn switchyard_provider_api::LiveInstance>>,
        ),
        ProviderError,
    > {
        // Try existing labelled worker first.
        if let Some(label) = recipe.label.as_deref()
            && let Some(inst) =
                self.registry
                    .checkout_by_label(&recipe.provider, recipe.session_id, label)
        {
            // checkout_by_label transitioned state to Busy. We still need the
            // instance_id, which checkout_by_label doesn't return; recover it
            // by listing the session and finding the busy entry with this
            // label. (Slice 1 trade-off — checkout_by_label could be improved
            // to return the id in a follow-up.)
            let id = self
                .registry
                .list_session(recipe.session_id)
                .into_iter()
                .find(|m| {
                    m.label.as_deref() == Some(label)
                        && m.provider == recipe.provider
                        && matches!(m.state, InstanceState::Busy { .. })
                })
                .map(|m| m.instance_id);
            if let Some(id) = id {
                return Ok((id, inst));
            }
            // Identity recovery failed — release and fall through to spawn.
            // (Should be unreachable in practice.)
        }

        // Spawn a fresh instance and register it. When the recipe
        // carries a resume_token (set on retry / explicit resume), we
        // route through the resumed variant so the daemon picks up
        // its prior thread instead of starting from scratch. The
        // resumed variant is responsible for graceful fallback when
        // the token is stale.
        let inst = match recipe.resume_token.clone() {
            Some(token) => {
                persistent
                    .start_persistent_instance_resumed(
                        recipe.cwd.clone(),
                        recipe.env.clone(),
                        Some(token),
                    )
                    .await?
            }
            None => {
                persistent
                    .start_persistent_instance(recipe.cwd.clone(), recipe.env.clone())
                    .await?
            }
        };

        let mut metadata = InstanceMetadata::new(
            recipe.provider.clone(),
            recipe.session_id,
            recipe.label.clone(),
            InstanceKind::Worker,
        );
        metadata.state = InstanceState::Idle;
        let spawned_at = metadata.spawned_at;
        let id = self
            .registry
            .register(metadata, inst)
            .map_err(|e| ProviderError::ExecutionFailed(format!("register failed: {e}")))?;

        // Announce the new worker before any state transitions land. UI
        // subscribers append it to the roster; the subsequent StateChanged
        // event (emitted by `execute`) flips it from idle to busy.
        self.emit(SupervisorLifecycleEvent::Spawned {
            session_id: recipe.session_id,
            instance_id: id,
            provider: recipe.provider.clone(),
            label: recipe.label.clone(),
            kind: "worker".to_string(),
            spawned_at: spawned_at.to_rfc3339(),
        });

        // Immediately transition to Busy via checkout_by_id so callers
        // observe consistent state.
        let inst_handle = self.registry.checkout_by_id(id).ok_or_else(|| {
            ProviderError::ExecutionFailed("freshly registered instance vanished".into())
        })?;
        Ok((id, inst_handle))
    }

    /// Run a single turn against an already-acquired worker. Returns the
    /// drained response text plus a verdict about whether the worker is still
    /// alive.
    #[allow(clippy::too_many_arguments)]
    async fn run_attempt(
        &self,
        instance_id: Uuid,
        inst_lock: std::sync::Arc<tokio::sync::Mutex<dyn switchyard_provider_api::LiveInstance>>,
        turn_id: Uuid,
        recipe: &SpawnRecipe,
        input_text: &str,
        context: ContextBundle,
        event_tx: &mpsc::Sender<ProviderEvent>,
        policy: &ExecutionPolicy,
        cancel: CancellationToken,
    ) -> AttemptResult {
        // Update context and start the turn.
        let mut event_rx = {
            let mut inst = inst_lock.lock().await;
            if let Err(e) = inst.update_context(context).await {
                return AttemptResult::DiedMidTurn {
                    error: format!("update_context failed: {e}"),
                };
            }
            match inst.send_message_with_policy(input_text, policy).await {
                Ok(rx) => rx,
                Err(e) => {
                    return AttemptResult::DiedMidTurn {
                        error: format!("send_message failed: {e}"),
                    };
                }
            }
        };

        // Drain events with stamping and turn-failure tracking.
        let mut response_text = String::new();
        let mut in_turn_failure = false;
        let mut died_unexpectedly = false;
        let mut saw_completion = false;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    return AttemptResult::DiedMidTurn {
                        error: "cancelled by user during turn".into(),
                    };
                }
                pe_opt = event_rx.recv() => {
                    match pe_opt {
                        Some(mut pe) => {
                            pe.turn_id = turn_id;
                            pe.instance_id = Some(instance_id);
                            pe.label = recipe.label.clone();
                            match pe.event_type {
                                EventType::TurnFailed => in_turn_failure = true,
                                EventType::TurnCompleted => saw_completion = true,
                                _ => {}
                            }
                            if let Some(text) = pe.payload.get("text").and_then(|t| t.as_str()) {
                                response_text.push_str(text);
                            } else if let Some(result_text) = pe.payload.get("result").and_then(|r| r.as_str()) {
                                response_text.push_str(result_text);
                            }
                            // Best-effort forward; if downstream gave up we
                            // still keep draining so the worker doesn't
                            // back-pressure to death.
                            let _ = event_tx.send(pe).await;
                        }
                        None => {
                            // Channel closed. If we never saw an explicit
                            // turn boundary (Completed or Failed), the worker
                            // process likely died mid-turn — trigger retry.
                            if !saw_completion && !in_turn_failure {
                                died_unexpectedly = true;
                            }
                            break;
                        }
                    }
                }
            }
        }

        if died_unexpectedly {
            // partial_response is intentionally discarded here — partial
            // output mid-flight is more confusing than helpful for the Core
            // when we're about to replay.
            return AttemptResult::DiedMidTurn {
                error: "worker stdout closed before turn produced output".into(),
            };
        }
        AttemptResult::Completed {
            response_text,
            in_turn_failure,
        }
    }

    /// Synthesize a `TurnFailed` event with `error_kind=worker_died_permanently`
    /// and emit it through the event channel. Returns the matching outcome.
    async fn emit_permanent_failure(
        &self,
        recipe: &SpawnRecipe,
        turn_id: Uuid,
        event_tx: &mpsc::Sender<ProviderEvent>,
        attempts: u32,
        last_error: Option<String>,
        input_text: &str,
    ) -> SupervisedOutcome {
        let lost_preview: String = input_text.chars().take(LOST_INPUT_PREVIEW_CHARS).collect();
        let payload = json!({
            "item_type": "delegate_result",
            "error_kind": "worker_died_permanently",
            "retries_attempted": attempts,
            "last_error": last_error.clone().unwrap_or_else(|| "unknown".to_string()),
            "lost_input_preview": lost_preview,
            "advice": "Worker died after retries exhausted. Consider re-delegating with a different provider, simplifying the task, or surfacing to the user.",
            "provider": recipe.provider.clone(),
            "label": recipe.label.clone(),
        });
        let mut event =
            ProviderEvent::new(turn_id, EventType::TurnFailed, &recipe.provider, payload);
        event.label = recipe.label.clone();
        let _ = event_tx.send(event).await;

        SupervisedOutcome {
            response_text: String::new(),
            failed: true,
            retries_attempted: attempts,
            last_error,
            final_instance_id: None,
        }
    }
}

/// Per-attempt result tracked by the retry loop.
enum AttemptResult {
    Completed {
        response_text: String,
        /// True when the provider emitted a `TurnFailed` event during this
        /// attempt (semantic failure — task didn't pan out). This is NOT a
        /// process-level death; the worker is still alive.
        in_turn_failure: bool,
    },
    DiedMidTurn {
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;
    use switchyard_provider_api::InstancePool;
    use switchyard_provider_api::{
        ArtifactBundle, CancellationToken, ContextBundle, ExecutionPolicy, LiveInstance,
        PersistentProvider, ProbeResult, Provider, ProviderError, ProviderEvent, TurnInput,
        TurnResult,
    };
    use tokio::sync::{Mutex, mpsc};

    /// A worker that produces a fixed-length text response and either:
    /// - succeeds (script == Success),
    /// - closes stdout early (script == DieMidTurn),
    /// - cycles through a Vec of scripts across successive spawns (used to
    ///   test retry semantics).
    #[derive(Clone)]
    enum Script {
        Success(String),
        DieMidTurn,
    }

    struct ScriptedInstance {
        script: Script,
    }

    #[async_trait]
    impl LiveInstance for ScriptedInstance {
        async fn send_message(
            &mut self,
            _text: &str,
        ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
            let (tx, rx) = mpsc::channel(8);
            match self.script.clone() {
                Script::Success(text) => {
                    let mut completion = ProviderEvent::new(
                        Uuid::nil(),
                        EventType::TurnCompleted,
                        "scripted",
                        json!({ "item_type": "delegate_result", "result": text.clone() }),
                    );
                    completion.payload["text"] = json!(text);
                    let _ = tx.send(completion).await;
                }
                Script::DieMidTurn => {
                    // Drop tx immediately to simulate stdout EOF before any
                    // event arrives.
                }
            }
            Ok(rx)
        }
        async fn update_context(&mut self, _ctx: ContextBundle) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn terminate(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    /// Provider that hands out instances by popping from a shared queue.
    /// Each call to `start_persistent_instance` removes the front item.
    struct ScriptedProvider {
        scripts: Arc<Mutex<Vec<Script>>>,
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        async fn probe(&self) -> Result<ProbeResult, ProviderError> {
            Ok(ProbeResult {
                available: true,
                ..Default::default()
            })
        }
        async fn start_turn(
            &self,
            _turn_id: Uuid,
            _input: TurnInput,
            _policy: ExecutionPolicy,
            _context: ContextBundle,
            _event_tx: mpsc::Sender<ProviderEvent>,
            _cancel: CancellationToken,
        ) -> Result<(), ProviderError> {
            unreachable!("supervisor uses start_persistent_instance, not start_turn")
        }
        async fn finalize_turn(
            &self,
            _turn_id: Uuid,
        ) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
            unreachable!()
        }
        fn as_persistent(&self) -> Option<&dyn PersistentProvider> {
            Some(self)
        }
    }

    #[async_trait]
    impl PersistentProvider for ScriptedProvider {
        async fn start_persistent_instance(
            &self,
            _cwd: PathBuf,
            _envs: HashMap<String, String>,
        ) -> Result<Box<dyn LiveInstance>, ProviderError> {
            let mut scripts = self.scripts.lock().await;
            if scripts.is_empty() {
                return Err(ProviderError::ExecutionFailed("script queue empty".into()));
            }
            let script = scripts.remove(0);
            Ok(Box::new(ScriptedInstance { script }))
        }
    }

    fn fast_policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            backoff: vec![Duration::ZERO, Duration::ZERO, Duration::ZERO],
        }
    }

    fn recipe(session: Uuid, label: Option<&str>) -> SpawnRecipe {
        SpawnRecipe {
            provider: "scripted".to_string(),
            session_id: session,
            label: label.map(|s| s.to_string()),
            cwd: PathBuf::from("."),
            env: HashMap::new(),
            reuse_after: true,
            resume_token: None,
        }
    }

    #[tokio::test]
    async fn success_on_first_attempt() {
        let pool = Arc::new(InstancePool::new());
        let supervisor = WorkerSupervisor::new(pool.clone(), fast_policy(), None);
        let provider = ScriptedProvider {
            scripts: Arc::new(Mutex::new(vec![Script::Success("hello".into())])),
        };
        let (tx, mut rx) = mpsc::channel(16);
        let session = Uuid::now_v7();
        let outcome = supervisor
            .execute(
                &provider,
                recipe(session, Some("worker-a")),
                Uuid::now_v7(),
                "do work".into(),
                ContextBundle {
                    summary: None,
                    recent_turns: vec![],
                    peer_state: vec![],
                    artifacts: vec![],
                },
                tx,
                ExecutionPolicy::workspace_write("."),
                CancellationToken::new(),
            )
            .await;

        assert!(!outcome.failed);
        assert_eq!(outcome.retries_attempted, 0);
        assert_eq!(outcome.response_text, "hello");
        assert!(outcome.final_instance_id.is_some());

        // The TurnCompleted event went through, stamped with label.
        let mut saw_completion = false;
        while let Ok(evt) = rx.try_recv() {
            if evt.event_type == EventType::TurnCompleted {
                assert_eq!(evt.label.as_deref(), Some("worker-a"));
                saw_completion = true;
            }
        }
        assert!(saw_completion);
    }

    #[tokio::test]
    async fn retries_then_succeeds() {
        let pool = Arc::new(InstancePool::new());
        let supervisor = WorkerSupervisor::new(pool.clone(), fast_policy(), None);
        let provider = ScriptedProvider {
            scripts: Arc::new(Mutex::new(vec![
                Script::DieMidTurn,
                Script::Success("recovered".into()),
            ])),
        };
        let (tx, _rx) = mpsc::channel(16);
        let outcome = supervisor
            .execute(
                &provider,
                recipe(Uuid::now_v7(), Some("worker-retry")),
                Uuid::now_v7(),
                "do work".into(),
                ContextBundle {
                    summary: None,
                    recent_turns: vec![],
                    peer_state: vec![],
                    artifacts: vec![],
                },
                tx,
                ExecutionPolicy::workspace_write("."),
                CancellationToken::new(),
            )
            .await;

        assert!(!outcome.failed);
        assert_eq!(outcome.retries_attempted, 1);
        assert_eq!(outcome.response_text, "recovered");
    }

    #[tokio::test]
    async fn permanent_death_emits_synthesized_failure() {
        let pool = Arc::new(InstancePool::new());
        let supervisor = WorkerSupervisor::new(pool.clone(), fast_policy(), None);
        let provider = ScriptedProvider {
            scripts: Arc::new(Mutex::new(vec![
                Script::DieMidTurn,
                Script::DieMidTurn,
                Script::DieMidTurn,
            ])),
        };
        let (tx, mut rx) = mpsc::channel(16);
        let outcome = supervisor
            .execute(
                &provider,
                recipe(Uuid::now_v7(), Some("doomed")),
                Uuid::now_v7(),
                "important task description that should appear in lost_input_preview".into(),
                ContextBundle {
                    summary: None,
                    recent_turns: vec![],
                    peer_state: vec![],
                    artifacts: vec![],
                },
                tx,
                ExecutionPolicy::workspace_write("."),
                CancellationToken::new(),
            )
            .await;

        assert!(outcome.failed);
        assert_eq!(outcome.retries_attempted, 3);
        assert!(outcome.final_instance_id.is_none());

        // Find the synthesized worker_died_permanently event.
        let mut saw_synth = false;
        while let Ok(evt) = rx.try_recv() {
            if evt.event_type == EventType::TurnFailed
                && evt.payload.get("error_kind").and_then(|v| v.as_str())
                    == Some("worker_died_permanently")
            {
                assert_eq!(evt.label.as_deref(), Some("doomed"));
                assert_eq!(evt.payload["retries_attempted"].as_u64(), Some(3));
                let preview = evt.payload["lost_input_preview"].as_str().unwrap_or("");
                assert!(preview.contains("important task description"));
                saw_synth = true;
            }
        }
        assert!(
            saw_synth,
            "supervisor must synthesize a worker_died_permanently event after retries exhausted",
        );
    }

    #[tokio::test]
    async fn reuse_after_false_terminates_after_success() {
        let pool = Arc::new(InstancePool::new());
        let supervisor = WorkerSupervisor::new(pool.clone(), fast_policy(), None);
        let provider = ScriptedProvider {
            scripts: Arc::new(Mutex::new(vec![Script::Success("done".into())])),
        };
        let (tx, _rx) = mpsc::channel(16);
        let mut rec = recipe(Uuid::now_v7(), Some("single-use"));
        rec.reuse_after = false;
        let session = rec.session_id;
        let outcome = supervisor
            .execute(
                &provider,
                rec,
                Uuid::now_v7(),
                "one-shot".into(),
                ContextBundle {
                    summary: None,
                    recent_turns: vec![],
                    peer_state: vec![],
                    artifacts: vec![],
                },
                tx,
                ExecutionPolicy::workspace_write("."),
                CancellationToken::new(),
            )
            .await;

        assert!(!outcome.failed);
        assert!(outcome.final_instance_id.is_none());
        assert!(pool.list_session(session).is_empty());
    }
}
