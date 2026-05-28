//! Unified runtime events for UI observation.
//!
//! These events are emitted during turn/router/orchestrator execution
//! so the TUI (or any observer) can render live state without polling the store.

use switchyard_provider_api::{ExecutionTelemetry, HyardJobObservation};
use uuid::Uuid;

use serde::{Deserialize, Serialize};

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum RuntimeEvent {
    /// Unread callback receipts were injected into the next routed provider-facing turn.
    CallbackReceiptsInjected {
        session_id: Uuid,
        provider: String,
        count: usize,
    },
    /// GUI/backend pre-flight work started before the canonical turn exists.
    /// This covers slow steps such as persistent CLI warm-start/resume and peer
    /// catalog probing, so frontends can show progress instead of waiting for
    /// the later `CoreTurnStarted` event.
    TurnPreparing {
        session_id: Uuid,
        provider: String,
        phase: String,
    },
    /// Core provider turn started.
    CoreTurnStarted {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
    },
    /// Core provider execution command resolved.
    CoreExecutionTelemetry {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
        execution: ExecutionTelemetry,
    },
    /// Core provider emitted a streaming item (text chunk, JSON event, etc.).
    CoreItemUpdated {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
        /// Canonical provider event type, e.g. `item_started`, `item_updated`,
        /// `item_completed`, or `artifact_ready`. Frontends need this to render
        /// tool lifecycles consistently instead of treating every live update as
        /// a generic `item_updated` snapshot.
        event_type: String,
        text: String,
        payload: Option<serde_json::Value>,
    },
    /// Raw terminal line mirrored from the core provider subprocess transport.
    CoreTerminalOutput {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
        text: String,
        transport: Option<String>,
    },
    /// Router detected a delegate request from the core's response.
    DelegateRequested {
        session_id: Uuid,
        core_turn_id: Uuid,
        peer: String,
        role: String,
        task_summary: String,
    },
    /// Peer provider turn started.
    PeerTurnStarted {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
    },
    /// Peer provider execution command resolved.
    PeerExecutionTelemetry {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
        execution: ExecutionTelemetry,
    },
    /// Peer provider emitted a streaming item.
    PeerItemUpdated {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
        /// Canonical provider event type, e.g. `item_started`, `item_updated`,
        /// `item_completed`, or `artifact_ready`.
        event_type: String,
        text: String,
        payload: Option<serde_json::Value>,
    },
    /// Raw terminal line mirrored from the peer provider subprocess transport.
    PeerTerminalOutput {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
        text: String,
        transport: Option<String>,
    },
    /// Delegate completed (success or failure).
    DelegateCompleted {
        session_id: Uuid,
        core_turn_id: Uuid,
        peer: String,
        status: String,
        summary: Option<String>,
    },
    /// A HYARD bridge command surfaced a job snapshot (delegate/status/result/await/cancel).
    HyardJobObserved {
        session_id: Uuid,
        turn_id: Uuid,
        source_provider: String,
        observed_at: String,
        job: HyardJobObservation,
    },
    /// Core provider output is complete (CLI exited). Finalize/archive still pending.
    CoreOutputCompleted {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
    },
    /// Peer provider output is complete. Orchestrator finalize still pending.
    PeerOutputCompleted {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
    },
    /// Core finalization turn started (after delegate result injected).
    FinalizationStarted {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
    },
    /// Turn completed successfully.
    TurnCompleted {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
        response: Option<String>,
    },
    /// Turn failed.
    TurnFailed {
        session_id: Uuid,
        turn_id: Uuid,
        provider: String,
        error: String,
    },
    /// A persistent worker (Core or peer) was just registered in the pool.
    /// Frontend should append it to the session's worker roster.
    WorkerSpawned {
        session_id: Uuid,
        instance_id: Uuid,
        provider: String,
        label: Option<String>,
        /// "core" or "worker" — mirrors `InstanceKind` serialization.
        kind: String,
        spawned_at: String,
    },
    /// A worker's pool state transitioned. Sent on idle↔busy, retrying flips,
    /// and any other observable state mutation. UI updates the existing row
    /// in place.
    WorkerStateChanged {
        session_id: Uuid,
        instance_id: Uuid,
        state: String,
        in_flight_turn_id: Option<Uuid>,
    },
    /// Supervisor is about to retry a delegation after a mid-turn worker
    /// death. `attempt` is the 1-indexed retry number; `last_error` describes
    /// what killed the previous instance. The Core is intentionally NOT
    /// informed of retries — this event is for the UI's benefit only.
    WorkerRetrying {
        session_id: Uuid,
        /// `None` if the previous spawn attempt itself failed (no instance id
        /// exists for the corpse).
        instance_id: Option<Uuid>,
        provider: String,
        label: Option<String>,
        attempt: u32,
        last_error: String,
    },
    /// A worker was removed from the pool. `reason` distinguishes graceful
    /// shutdown from mid-turn death so the UI can colour appropriately.
    WorkerTerminated {
        session_id: Uuid,
        instance_id: Uuid,
        provider: String,
        label: Option<String>,
        /// One of: `released`, `completed_use_once`, `died_mid_turn`,
        /// `permanent_death`, `core_reset`, `session_clear`.
        reason: String,
    },
}
