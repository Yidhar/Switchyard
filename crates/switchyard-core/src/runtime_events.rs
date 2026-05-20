//! Unified runtime events for UI observation.
//!
//! These events are emitted during turn/router/orchestrator execution
//! so the TUI (or any observer) can render live state without polling the store.

use switchyard_provider_api::{ExecutionTelemetry, HyardJobObservation};
use uuid::Uuid;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum RuntimeEvent {
    /// Unread callback receipts were injected into the next routed provider-facing turn.
    CallbackReceiptsInjected { provider: String, count: usize },
    /// Core provider turn started.
    CoreTurnStarted { turn_id: Uuid, provider: String },
    /// Core provider execution command resolved.
    CoreExecutionTelemetry {
        turn_id: Uuid,
        provider: String,
        execution: ExecutionTelemetry,
    },
    /// Core provider emitted a streaming item (text chunk, JSON event, etc.).
    CoreItemUpdated {
        turn_id: Uuid,
        provider: String,
        text: String,
        payload: Option<serde_json::Value>,
    },
    /// Raw terminal line mirrored from the core provider subprocess transport.
    CoreTerminalOutput {
        turn_id: Uuid,
        provider: String,
        text: String,
        transport: Option<String>,
    },
    /// Router detected a delegate request from the core's response.
    DelegateRequested {
        core_turn_id: Uuid,
        peer: String,
        role: String,
        task_summary: String,
    },
    /// Peer provider turn started.
    PeerTurnStarted { turn_id: Uuid, provider: String },
    /// Peer provider execution command resolved.
    PeerExecutionTelemetry {
        turn_id: Uuid,
        provider: String,
        execution: ExecutionTelemetry,
    },
    /// Peer provider emitted a streaming item.
    PeerItemUpdated {
        turn_id: Uuid,
        provider: String,
        text: String,
        payload: Option<serde_json::Value>,
    },
    /// Raw terminal line mirrored from the peer provider subprocess transport.
    PeerTerminalOutput {
        turn_id: Uuid,
        provider: String,
        text: String,
        transport: Option<String>,
    },
    /// Delegate completed (success or failure).
    DelegateCompleted {
        core_turn_id: Uuid,
        peer: String,
        status: String,
        summary: Option<String>,
    },
    /// A HYARD bridge command surfaced a job snapshot (delegate/status/result/await/cancel).
    HyardJobObserved {
        source_provider: String,
        observed_at: String,
        job: HyardJobObservation,
    },
    /// Core provider output is complete (CLI exited). Finalize/archive still pending.
    CoreOutputCompleted { turn_id: Uuid, provider: String },
    /// Peer provider output is complete. Orchestrator finalize still pending.
    PeerOutputCompleted { turn_id: Uuid, provider: String },
    /// Core finalization turn started (after delegate result injected).
    FinalizationStarted { turn_id: Uuid, provider: String },
    /// Turn completed successfully.
    TurnCompleted {
        turn_id: Uuid,
        provider: String,
        response: Option<String>,
    },
    /// Turn failed.
    TurnFailed {
        turn_id: Uuid,
        provider: String,
        error: String,
    },
}
